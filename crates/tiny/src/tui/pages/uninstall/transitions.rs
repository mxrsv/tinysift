//! How the page moves between phases: reading the list and the plans,
//! freezing the picks, running and ending the removal.

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;

use tiny_core::uninstall::{sort_apps, AppEntry};

use super::engine::UninstallEngine;
use super::keys;
use super::model::{figures, AppRow};
use super::run::Run;
use super::tasks::{self, FreezeOutcome, FrozenApp, Msg};
use super::{boxes, Phase, PlanRead, UninstallPage};
use crate::tui::pages::PageCtx;

impl<E: UninstallEngine> UninstallPage<E> {
    /// Reads `/Applications` again; picks of apps still listed survive.
    pub(super) fn read(&mut self, ctx: &mut PageCtx<'_, Msg<E::Frozen>>) {
        let ticket = self.ticket();
        self.list_ticket = ticket;
        self.plans.clear();
        self.plan_task = None;
        self.phase = match ctx.spawn(tasks::list(self.engine.clone(), ticket)) {
            Ok(task) => Phase::Reading { ticket, task },
            Err(error) => Phase::Unreadable(error.to_string()),
        };
    }

    /// A read (the first, or `r`) opens on the first row of the list as
    /// sorted that can be ticked; only `s` keeps the cursor on its app.
    pub(super) fn on_listed(&mut self, rows: Vec<AppRow>, ctx: &mut PageCtx<'_, Msg<E::Frozen>>) {
        let listed: BTreeSet<&PathBuf> = rows
            .iter()
            .filter(|row| !row.refused())
            .map(|row| &row.app.path)
            .collect();
        self.ticked.retain(|path| listed.contains(path));
        self.apps = rows;
        self.resort();
        let first = self.apps.iter().position(|row| !row.refused()).unwrap_or(0);
        self.cursor = keys::cursor_at(first, self.apps.len());
        self.phase = Phase::Browse;
        self.read_next_plan(ctx);
    }

    /// Orders the rows as `sift uninstall --sort` does, keeping the cursor on
    /// its app. Ties keep a fixed order, name then path (C6): the engine's
    /// sort is stable, so it is fed in that order.
    pub(super) fn resort(&mut self) {
        let current = self.highlighted().map(|row| row.app.path.clone());
        let mut rows: HashMap<PathBuf, AppRow> = std::mem::take(&mut self.apps)
            .into_iter()
            .map(|row| (row.app.path.clone(), row))
            .collect();
        let mut entries: Vec<AppEntry> = rows.values().map(|row| row.app.clone()).collect();
        entries.sort_by(|a, b| {
            (a.name.to_lowercase(), &a.name, &a.path).cmp(&(
                b.name.to_lowercase(),
                &b.name,
                &b.path,
            ))
        });
        self.apps = sort_apps(entries, &self.sort)
            .into_iter()
            .filter_map(|app| rows.remove(&app.path))
            .collect();
        if let Some(index) =
            current.and_then(|path| self.apps.iter().position(|row| row.app.path == path))
        {
            self.cursor = keys::cursor_at(index, self.apps.len());
        }
    }

    /// Starts the next plan read if none runs: the highlighted app first,
    /// then a picked app the foot still needs. At most one read is in flight,
    /// so a fast cursor never queues a pile of reads.
    pub(super) fn read_next_plan(&mut self, ctx: &mut PageCtx<'_, Msg<E::Frozen>>) {
        if self.plan_task.is_some() || !matches!(self.phase, Phase::Browse) {
            return;
        }
        let unread = |row: &&AppRow| !row.refused() && !self.plans.contains_key(&row.app.path);
        let next = self
            .highlighted()
            .filter(unread)
            .or_else(|| self.picked().into_iter().find(unread))
            .map(|row| row.app.clone());
        let Some(app) = next else {
            return;
        };
        let path = app.path.clone();
        let job = tasks::plan(self.engine.clone(), self.list_ticket, app, self.installed());
        match ctx.spawn(job) {
            Ok(task) => self.plan_task = Some((path, task.id())),
            Err(error) => {
                self.plans.insert(path, PlanRead::Failed(error.to_string()));
            }
        }
    }

    /// Closes a box or the dry-run list without acting; picks stay.
    pub(super) fn back_to_list(&mut self, ctx: &mut PageCtx<'_, Msg<E::Frozen>>) {
        self.phase = Phase::Browse;
        self.read_next_plan(ctx);
    }

    /// Builds and freezes the picked plans; the menu opens when they are frozen.
    pub(super) fn freeze(&mut self, ctx: &mut PageCtx<'_, Msg<E::Frozen>>) {
        let picked: Vec<AppEntry> = self
            .picked()
            .into_iter()
            .map(|row| row.app.clone())
            .collect();
        if picked.is_empty() {
            return;
        }
        let ticket = self.ticket();
        let job = tasks::freeze(
            self.engine.clone(),
            ticket,
            picked.clone(),
            self.installed(),
        );
        self.phase = match ctx.spawn(job) {
            Ok(task) => Phase::Freezing { ticket, task },
            Err(error) => Phase::Done(Run::refused(
                picked
                    .into_iter()
                    .map(|app| (app.name, 0, Some(error.to_string())))
                    .collect(),
            )),
        };
    }

    pub(super) fn on_frozen(&mut self, apps: Vec<FreezeOutcome<E::Frozen>>) {
        if apps.iter().all(Result::is_ok) {
            let batch: Vec<_> = apps.into_iter().filter_map(Result::ok).collect();
            let menu = boxes::action_menu(&batch);
            self.phase = Phase::Menu { batch, menu };
            return;
        }
        // As the command does: one refusal and nothing is removed.
        let lines = apps
            .into_iter()
            .map(|app| match app {
                Ok(app) => (
                    app.plan.app.name.clone(),
                    figures(&app.plan).removable,
                    None,
                ),
                Err((name, reason)) => (name, 0, Some(reason)),
            })
            .collect();
        self.phase = Phase::Done(Run::refused(lines));
    }

    /// Removes the frozen apps, to the Trash or for good.
    pub(super) fn start_run(
        &mut self,
        batch: Vec<FrozenApp<E::Frozen>>,
        hard: bool,
        ctx: &mut PageCtx<'_, Msg<E::Frozen>>,
    ) {
        let run = Run::new(
            batch
                .iter()
                .map(|app| (app.plan.app.name.clone(), figures(&app.plan).removable))
                .collect(),
            hard,
        );
        self.phase = match ctx.spawn_write(tasks::run(self.engine.clone(), batch, hard)) {
            Ok(task) => Phase::Running { run, task },
            Err(error) => Phase::Done(Run::refused(
                run.lines
                    .into_iter()
                    .map(|line| (line.name, line.bytes, Some(error.to_string())))
                    .collect(),
            )),
        };
    }

    /// The run task ended: report it, and read the free space again.
    pub(super) fn end_run(&mut self, reason: Option<&str>, ctx: &mut PageCtx<'_, Msg<E::Frozen>>) {
        if !matches!(self.phase, Phase::Running { .. }) {
            return;
        }
        if let Phase::Running { run, .. } = std::mem::replace(&mut self.phase, Phase::Browse) {
            self.phase = Phase::Done(run.end(reason));
            ctx.refresh_free_space();
        }
    }
}
