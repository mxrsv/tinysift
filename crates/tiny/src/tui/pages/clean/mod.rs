//! Clean: the categories `sift clean` lists, Safe only until `v` (TU8), the
//! path list, the action and hard-delete boxes (TU9), the running ✓ list
//! (TU3), the done screen (TU11) and the exit card (TU4).
//!
//! The engine is reached only through [`engine::CleanEngine`]; every rule
//! about what may be listed, ticked or removed is the engine's.

mod boxes;
mod engine;
mod exit_card;
mod keys;
mod list_view;
mod listing;
mod paths;
mod run;
mod run_view;
mod wording;

#[cfg(test)]
mod fake_data;
#[cfg(test)]
mod fake_engine;
#[cfg(test)]
mod tests;

use std::ops::ControlFlow;
use std::sync::Arc;
use std::time::Instant;

use crossterm::event::KeyEvent;
use tiny_core::clean::execute::{GroupOutcome, ObservedRun};
use tiny_core::clean::types::CleanAction;

use super::{KeyReply, Page, PageCtx, PageId, PageView, ViewCtx};
use crate::tui::styled_line::StyledLine;
use crate::tui::widgets::action_menu::ActionMenu;
use crate::tui::widgets::confirm_box::ConfirmBox;
use crate::tui::widgets::list_cursor::ListCursor;
use crate::tui::work::{TaskHandle, Worker};

use engine::{CleanEngine, CoreEngine, Discovery};
use list_view::{ListScreen, Measuring};
use listing::Listing;
use paths::PathList;
use run::{Run, Session};

/// Reads the time, so tests can set it.
pub type Clock = Arc<dyn Fn() -> Instant + Send + Sync>;

pub fn page() -> CleanPage {
    CleanPage::new(Arc::new(CoreEngine), Arc::new(Instant::now))
}

/// What the page's tasks send back. `read` tells a current read from one
/// that was stopped or replaced.
pub enum Msg {
    Progress { read: u64, done: u64, total: u64 },
    Read { read: u64, discovery: Discovery },
    Group(GroupOutcome),
    RunEnded(Result<ObservedRun, String>),
}

enum Stage {
    Reading {
        read: u64,
        task: Option<TaskHandle>,
        progress: Measuring,
    },
    Stopped,
    Listed,
    Paths(PathList),
    Menu(ActionMenu),
    Confirm(ConfirmBox),
    DryRun(ListCursor),
    Running(Run),
    Done(Run),
}

pub struct CleanPage {
    engine: Arc<dyn CleanEngine>,
    clock: Clock,
    review: bool,
    reads: u64,
    opened: bool,
    listing: Option<Listing>,
    cursor: ListCursor,
    stage: Stage,
    session: Session,
}

impl CleanPage {
    pub fn new(engine: Arc<dyn CleanEngine>, clock: Clock) -> Self {
        Self {
            engine,
            clock,
            review: false,
            reads: 0,
            opened: false,
            listing: None,
            cursor: ListCursor::default(),
            stage: Stage::Stopped,
            session: Session::default(),
        }
    }

    fn screen(&self) -> ListScreen<'_> {
        ListScreen {
            listing: self.listing.as_ref(),
            cursor: self.cursor.index(),
            review: self.review,
        }
    }

    /// Measures again; the rows of the last read stay on screen, frozen, until it ends.
    fn start_read(&mut self, ctx: &mut PageCtx<'_, Msg>) {
        self.cancel_read();
        self.reads += 1;
        let read = self.reads;
        let review = self.review;
        let engine = self.engine.clone();
        let spawned = ctx.spawn(move |worker: Worker<Msg>| {
            let progress = |done, total| {
                worker.send(Msg::Progress { read, done, total });
            };
            let discovery = engine.discover(review, &progress, worker.cancel_flag());
            worker.send(Msg::Read { read, discovery });
        });
        match spawned {
            Ok(task) => {
                self.stage = Stage::Reading {
                    read,
                    task: Some(task),
                    progress: Measuring { done: 0, total: 0 },
                }
            }
            Err(error) => self.fail_read(format!("could not start measuring: {error}")),
        }
    }

    fn cancel_read(&mut self) {
        if let Stage::Reading {
            task: Some(task), ..
        } = &self.stage
        {
            task.cancel();
        }
    }

    fn fail_read(&mut self, reason: String) {
        self.listing = Some(Listing::new(Discovery {
            failure: Some(reason),
            ..Discovery::default()
        }));
        self.stage = Stage::Listed;
    }

    fn start_run(&mut self, action: CleanAction, ctx: &mut PageCtx<'_, Msg>) {
        let Some(listing) = &self.listing else { return };
        let plan = listing.plan(action);
        let mut run = Run::new(&plan, listing, (self.clock)());
        let engine = self.engine.clone();
        let spawned = ctx.spawn(move |worker: Worker<Msg>| {
            let result = engine.execute(&plan, &mut |outcome| {
                let open = worker.send(Msg::Group(outcome.clone()));
                // Stop is checked between categories only: one in progress always finishes (I14).
                if open && !worker.is_cancelled() {
                    ControlFlow::Continue(())
                } else {
                    ControlFlow::Break(())
                }
            });
            worker.send(Msg::RunEnded(result));
        });
        match spawned {
            Ok(task) => run.task = Some(task),
            Err(error) => run.end(
                Err(format!("could not start cleaning: {error}")),
                Default::default(),
            ),
        }
        self.stage = if run.ended() {
            Stage::Done(run)
        } else {
            Stage::Running(run)
        };
    }

    fn stop_run(&mut self) {
        if let Stage::Running(run) = &mut self.stage {
            run.stopping = true;
            if let Some(task) = &run.task {
                task.cancel();
            }
        }
    }

    fn end_run(&mut self, result: Result<ObservedRun, String>, ctx: &mut PageCtx<'_, Msg>) {
        let Stage::Running(mut run) = std::mem::replace(&mut self.stage, Stage::Listed) else {
            return;
        };
        run.end(
            result,
            (self.clock)().saturating_duration_since(run.started),
        );
        self.session.add(&run);
        ctx.refresh_free_space();
        self.stage = Stage::Done(run);
    }
}

impl Page for CleanPage {
    const ID: PageId = PageId::Clean;
    type Msg = Msg;

    fn view(&self, ctx: &ViewCtx) -> PageView {
        let screen = self.screen();
        match &self.stage {
            Stage::Reading { progress, .. } => screen.measuring(*progress, ctx),
            Stage::Stopped => screen.stopped(),
            Stage::Listed => screen.listed(ctx),
            Stage::Paths(paths) => self
                .listing
                .as_ref()
                .map_or_else(PageView::default, |l| paths.view(l, ctx)),
            Stage::Menu(menu) => PageView {
                keys: ActionMenu::key_hints(),
                overlay: Some(menu.overlay()),
                ..screen.listed(ctx)
            },
            Stage::Confirm(confirm) => PageView {
                keys: ConfirmBox::key_hints(),
                overlay: Some(confirm.overlay()),
                ..screen.listed(ctx)
            },
            Stage::DryRun(cursor) => self.listing.as_ref().map_or_else(PageView::default, |l| {
                boxes::dry_run(l, cursor.index(), ctx)
            }),
            Stage::Running(run) => run_view::running(
                run,
                (self.clock)().saturating_duration_since(run.started),
                ctx,
            ),
            Stage::Done(run) => run_view::done(
                run,
                self.listing.as_ref().and_then(|l| l.home.as_ref()),
                ctx,
            ),
        }
    }

    fn on_show(&mut self, ctx: &mut PageCtx<'_, Msg>) {
        if !self.opened {
            self.opened = true;
            self.start_read(ctx);
        }
    }

    fn on_reload(&mut self, ctx: &mut PageCtx<'_, Msg>) {
        if !matches!(self.stage, Stage::Running(_)) {
            self.opened = true;
            self.start_read(ctx);
        }
    }

    fn on_key(&mut self, key: KeyEvent, ctx: &mut PageCtx<'_, Msg>) -> KeyReply {
        self.handle_key(key, ctx)
    }

    fn on_message(&mut self, message: Msg, ctx: &mut PageCtx<'_, Msg>) {
        match message {
            Msg::Progress { read, done, total } => {
                if let Stage::Reading {
                    read: current,
                    progress,
                    ..
                } = &mut self.stage
                {
                    if *current == read {
                        *progress = Measuring { done, total };
                    }
                }
            }
            Msg::Read { read, discovery } => {
                if matches!(self.stage, Stage::Reading { read: current, .. } if current == read) {
                    self.listing = Some(Listing::new(discovery));
                    self.cursor = ListCursor::default();
                    self.stage = Stage::Listed;
                }
            }
            Msg::Group(outcome) => {
                if let Stage::Running(run) = &mut self.stage {
                    run.record(&outcome);
                }
            }
            Msg::RunEnded(result) => self.end_run(result, ctx),
        }
    }

    fn on_task_failed(&mut self, reason: &str, ctx: &mut PageCtx<'_, Msg>) {
        match self.stage {
            Stage::Running(_) => self.end_run(Err(reason.to_string()), ctx),
            Stage::Reading { .. } => self.fail_read(reason.to_string()),
            _ => {}
        }
    }

    fn is_busy(&self) -> bool {
        matches!(self.stage, Stage::Reading { .. } | Stage::Running(_))
    }

    fn on_quit_request(&mut self, _ctx: &mut PageCtx<'_, Msg>) {
        match self.stage {
            Stage::Running(_) => self.stop_run(),
            Stage::Reading { .. } => self.cancel_read(),
            Stage::Menu(_) | Stage::Confirm(_) => self.stage = Stage::Listed,
            _ => {}
        }
    }

    fn holds_exit(&self) -> bool {
        matches!(self.stage, Stage::Running(_))
    }

    fn exit_card(&self) -> Option<Vec<StyledLine>> {
        exit_card::exit_card(&self.session)
    }
}
