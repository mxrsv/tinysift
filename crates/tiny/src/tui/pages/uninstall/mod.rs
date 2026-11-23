//! Uninstall: the apps in `/Applications`, removed with the data they left in
//! `~/Library` (spec TU9, I16).
//!
//! The list is read when the tab first shows; nothing is ticked. Moving the
//! cursor reads the highlighted app's plan, one read at a time. Enter builds
//! and freezes the picked apps' plans, then opens the action menu; the run
//! removes app by app through the engine's checked executor. Everything the
//! page knows of the engine comes through [`engine::UninstallEngine`].

mod boxes;
mod browse_view;
mod engine;
mod keys;
mod model;
mod run;
mod run_view;
mod tasks;
mod transitions;

#[cfg(test)]
mod consent_tests;
#[cfg(test)]
mod fake_engine;
#[cfg(test)]
mod fake_gate;
#[cfg(test)]
mod flow_tests;
#[cfg(test)]
mod progress_tests;
#[cfg(test)]
mod snapshot_tests;

use std::collections::{BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::Arc;

use tiny_core::options::SortBy;
use tiny_core::uninstall::{AppEntry, Plan};

use super::{Page, PageCtx, PageId, PageView, ViewCtx};
use crate::tui::widgets::action_menu::ActionMenu;
use crate::tui::widgets::confirm_box::ConfirmBox;
use crate::tui::widgets::list_cursor::ListCursor;
use crate::tui::work::{TaskHandle, TaskId};
use engine::{CoreEngine, UninstallEngine};
use model::AppRow;
use run::Run;
use tasks::{FrozenApp, Msg};

pub fn page() -> UninstallPage<CoreEngine> {
    UninstallPage::new(Arc::new(CoreEngine))
}

/// A plan read for one app.
enum PlanRead {
    Ready(Plan),
    Failed(String),
}

/// What the page is doing. Boxes and runs own the frozen plans they act on.
enum Phase<F> {
    /// The tab has not been shown yet.
    Unread,
    Reading {
        ticket: u64,
        task: TaskHandle,
    },
    /// esc stopped the read; `r` reads again.
    Stopped,
    /// `/Applications` could not be read: the engine's words.
    Unreadable(String),
    /// The list (possibly empty).
    Browse,
    /// Enter was pressed: the picked plans are being built and frozen.
    Freezing {
        ticket: u64,
        task: TaskHandle,
    },
    Menu {
        batch: Vec<FrozenApp<F>>,
        menu: ActionMenu,
    },
    Confirm {
        batch: Vec<FrozenApp<F>>,
        confirm: ConfirmBox,
    },
    DryRun {
        batch: Vec<FrozenApp<F>>,
        cursor: ListCursor,
    },
    Running {
        run: Run,
        task: TaskHandle,
    },
    Done(Run),
}

pub struct UninstallPage<E: UninstallEngine> {
    engine: Arc<E>,
    /// `~/Library`, cut from the leftover paths shown.
    library: Option<PathBuf>,
    phase: Phase<E::Frozen>,
    apps: Vec<AppRow>,
    sort: SortBy,
    cursor: ListCursor,
    /// Picked apps, by bundle path.
    ticked: BTreeSet<PathBuf>,
    plans: HashMap<PathBuf, PlanRead>,
    /// The one plan read in flight: the app's bundle path and the task.
    plan_task: Option<(PathBuf, TaskId)>,
    /// The list read the shown rows and plans belong to.
    list_ticket: u64,
    next_ticket: u64,
}

impl<E: UninstallEngine> UninstallPage<E> {
    pub fn new(engine: Arc<E>) -> Self {
        Self {
            library: engine.library(),
            engine,
            phase: Phase::Unread,
            apps: Vec::new(),
            sort: SortBy::LastUsed,
            cursor: ListCursor::default(),
            ticked: BTreeSet::new(),
            plans: HashMap::new(),
            plan_task: None,
            list_ticket: 0,
            next_ticket: 0,
        }
    }

    fn ticket(&mut self) -> u64 {
        self.next_ticket += 1;
        self.next_ticket
    }

    fn highlighted(&self) -> Option<&AppRow> {
        self.apps.get(self.cursor.index())
    }

    fn installed(&self) -> Vec<AppEntry> {
        self.apps.iter().map(|row| row.app.clone()).collect()
    }

    /// Picked apps in list order.
    fn picked(&self) -> Vec<&AppRow> {
        self.apps
            .iter()
            .filter(|row| self.ticked.contains(&row.app.path))
            .collect()
    }
}

impl<E: UninstallEngine> Page for UninstallPage<E> {
    const ID: PageId = PageId::Uninstall;
    type Msg = Msg<E::Frozen>;

    fn view(&self, ctx: &ViewCtx) -> PageView {
        match &self.phase {
            Phase::Unread | Phase::Reading { .. } => self.reading_view(ctx),
            Phase::Stopped => self.notice_view(ctx, None),
            Phase::Unreadable(reason) => self.notice_view(ctx, Some(reason)),
            Phase::Browse if self.apps.is_empty() => self.empty_view(ctx),
            Phase::Browse => self.browse_view(ctx),
            Phase::Freezing { .. } => self.freezing_view(ctx),
            Phase::Menu { menu, .. } => PageView {
                keys: ActionMenu::key_hints(),
                overlay: Some(menu.overlay()),
                ..self.browse_view(ctx)
            },
            Phase::Confirm { confirm, .. } => PageView {
                keys: ConfirmBox::key_hints(),
                overlay: Some(confirm.overlay()),
                ..self.browse_view(ctx)
            },
            Phase::DryRun { batch, cursor } => self.dry_run_view(ctx, batch, cursor),
            Phase::Running { run, .. } => self.run_view(ctx, run, true),
            Phase::Done(run) => self.run_view(ctx, run, false),
        }
    }

    fn on_show(&mut self, ctx: &mut PageCtx<'_, Self::Msg>) {
        if matches!(self.phase, Phase::Unread) {
            self.read(ctx);
        }
    }

    fn on_reload(&mut self, ctx: &mut PageCtx<'_, Self::Msg>) {
        if matches!(
            self.phase,
            Phase::Unread | Phase::Stopped | Phase::Unreadable(_) | Phase::Browse | Phase::Done(_)
        ) {
            self.read(ctx);
        }
    }

    fn on_key(
        &mut self,
        key: crossterm::event::KeyEvent,
        ctx: &mut PageCtx<'_, Self::Msg>,
    ) -> super::KeyReply {
        self.handle_key(key, ctx)
    }

    fn on_message(&mut self, message: Self::Msg, ctx: &mut PageCtx<'_, Self::Msg>) {
        match message {
            Msg::Listed { ticket, result } => {
                if !matches!(self.phase, Phase::Reading { ticket: t, .. } if t == ticket) {
                    return;
                }
                match result {
                    Ok(rows) => self.on_listed(rows, ctx),
                    Err(reason) => self.phase = Phase::Unreadable(reason),
                }
            }
            Msg::Planned {
                ticket,
                path,
                result,
            } => {
                if ticket != self.list_ticket {
                    return;
                }
                if self.plan_task.as_ref().is_some_and(|(p, _)| *p == path) {
                    self.plan_task = None;
                }
                let read = result.map_or_else(PlanRead::Failed, PlanRead::Ready);
                self.plans.insert(path, read);
                self.read_next_plan(ctx);
            }
            Msg::Frozen { ticket, apps } => {
                if matches!(self.phase, Phase::Freezing { ticket: t, .. } if t == ticket) {
                    self.on_frozen(apps);
                }
            }
            Msg::Started(index) => {
                if let Phase::Running { run, .. } = &mut self.phase {
                    run.start(index);
                }
            }
            Msg::Finished(index, result) => {
                if let Phase::Running { run, .. } = &mut self.phase {
                    run.finish(index, result);
                }
            }
            Msg::RunEnded => self.end_run(None, ctx),
        }
    }

    fn on_task_failed(&mut self, task: TaskId, reason: &str, ctx: &mut PageCtx<'_, Self::Msg>) {
        // Every job turns an engine panic into its own answer; this is the
        // fallback for one that ended without sending it. Only a task the
        // page still owns may change what it shows.
        if self.plan_task.as_ref().is_some_and(|(_, id)| *id == task) {
            if let Some((path, _)) = self.plan_task.take() {
                self.plans.insert(path, PlanRead::Failed(reason.into()));
            }
            return;
        }
        match &self.phase {
            Phase::Running { task: handle, .. } if handle.id() == task => {
                self.end_run(Some(reason), ctx)
            }
            Phase::Reading { task: handle, .. } if handle.id() == task => {
                self.phase = Phase::Unreadable(reason.into())
            }
            Phase::Freezing { task: handle, .. } if handle.id() == task => {
                let picked = self.picked().into_iter().map(|row| row.app.name.clone());
                let lines = picked
                    .map(|name| (name, 0, Some(reason.to_string())))
                    .collect();
                self.phase = Phase::Done(Run::refused(lines));
            }
            _ => {}
        }
    }

    fn is_busy(&self) -> bool {
        self.plan_task.is_some()
            || matches!(
                self.phase,
                Phase::Reading { .. } | Phase::Freezing { .. } | Phase::Running { .. }
            )
    }

    fn on_quit_request(&mut self, _ctx: &mut PageCtx<'_, Self::Msg>) {
        match &mut self.phase {
            Phase::Reading { task, .. } => {
                task.cancel();
                self.phase = Phase::Stopped;
            }
            Phase::Freezing { task, .. } => {
                task.cancel();
                self.phase = Phase::Browse;
            }
            Phase::Menu { .. } | Phase::Confirm { .. } | Phase::DryRun { .. } => {
                self.phase = Phase::Browse;
            }
            Phase::Running { run, task } => {
                task.cancel();
                run.stopping = true;
            }
            Phase::Unread
            | Phase::Stopped
            | Phase::Unreadable(_)
            | Phase::Browse
            | Phase::Done(_) => {}
        }
    }

    fn holds_exit(&self) -> bool {
        matches!(self.phase, Phase::Running { .. })
    }

    fn held_step(&self) -> &'static str {
        "app"
    }
}
