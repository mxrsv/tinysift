//! Processes: every process, read once when the tab first shows and again on
//! `r`; nothing refreshes by itself, so rows never move under the cursor (I18).
//! `x` quits and `X` force quits the highlighted process through the shared
//! confirmation box and the engine's guarded call (TU9).

mod engine;
mod view;

use std::collections::HashMap;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent};
use tiny_core::processes::{
    self, ProcessKey, ProcessSort, Refusal, TerminateKind, TerminateOutcome, TerminateTarget,
};

use super::{KeyHint, KeyReply, Page, PageCtx, PageId, PageView, ViewCtx};
use crate::tui::styled_line::StyledLine;
use crate::tui::widgets::confirm_box::{ConfirmBox, ConfirmReply};
use crate::tui::widgets::list_cursor::ListCursor;
use crate::tui::widgets::spinner::spinner_line;
use crate::tui::work::TaskId;
use engine::{ProcessEngine, ProcessRow, RealProcesses};

pub enum ProcessesMsg {
    Read(Vec<ProcessRow>),
    Quit {
        target: TerminateTarget,
        kind: TerminateKind,
        result: Result<TerminateOutcome, String>,
    },
}

/// An open confirmation box and the process it is about.
struct Pending {
    confirm: ConfirmBox,
    target: TerminateTarget,
    kind: TerminateKind,
}

pub struct ProcessesPage<E> {
    engine: Arc<E>,
    rows: Option<Vec<ProcessRow>>,
    sort: ProcessSort,
    cursor: ListCursor,
    /// The read in flight.
    reading: Option<TaskId>,
    /// Set when the first read fails: there is nothing else to show.
    failed: Option<String>,
    pending: Option<Pending>,
    /// The process a quit is running for, and its task.
    quitting: Option<(String, TaskId)>,
    /// The engine refused the highlighted process; shown in its detail.
    refused: Option<(ProcessKey, Refusal)>,
    /// The engine's answer to the last quit, shown on the foot until the next action.
    notice: Option<StyledLine>,
}

pub fn page() -> ProcessesPage<RealProcesses> {
    ProcessesPage::new(RealProcesses)
}

fn key_of(row: &ProcessRow) -> ProcessKey {
    ProcessKey {
        pid: row.info.pid,
        start_time: row.info.start_time,
    }
}

impl<E: ProcessEngine> ProcessesPage<E> {
    fn new(engine: E) -> Self {
        Self {
            engine: Arc::new(engine),
            rows: None,
            sort: ProcessSort::Cpu,
            cursor: ListCursor::default(),
            reading: None,
            failed: None,
            pending: None,
            quitting: None,
            refused: None,
            notice: None,
        }
    }

    fn highlighted(&self) -> Option<&ProcessRow> {
        self.rows.as_ref()?.get(self.cursor.index())
    }

    fn start_read(&mut self, ctx: &mut PageCtx<'_, ProcessesMsg>) {
        if self.reading.is_some() {
            return;
        }
        let engine = Arc::clone(&self.engine);
        match ctx.spawn(move |worker| {
            worker.send(ProcessesMsg::Read(engine.read()));
        }) {
            Ok(task) => self.reading = Some(task.id()),
            Err(error) => self.failed = Some(error.to_string()),
        }
    }

    /// Puts `rows` in the current order and keeps `kept` highlighted when it is still there.
    fn place(&mut self, rows: Vec<ProcessRow>, kept: Option<ProcessKey>) {
        let rows = sorted(rows, self.sort);
        let found = kept.and_then(|key| rows.iter().position(|row| key_of(row) == key));
        let len = rows.len();
        self.rows = Some(rows);
        match found {
            Some(index) => self.cursor.settle(len, |i| i == index),
            None => self.cursor.settle(len, |_| true),
        }
    }

    fn ask(&mut self, kind: TerminateKind) {
        if self.quitting.is_some() {
            return;
        }
        let Some(row) = self.highlighted() else {
            return;
        };
        let (key, refusal) = (key_of(row), self.engine.refusal(&row.info));
        let pending = refusal.is_none().then(|| Pending {
            confirm: view::confirm_box(&row.info, kind),
            target: TerminateTarget::from(&row.info),
            kind,
        });
        self.notice = None;
        self.refused = refusal.map(|reason| (key, reason));
        self.pending = pending;
    }

    fn quit(
        &mut self,
        target: TerminateTarget,
        kind: TerminateKind,
        ctx: &mut PageCtx<'_, ProcessesMsg>,
    ) {
        let engine = Arc::clone(&self.engine);
        let name = target.name.clone();
        let spawned = ctx.spawn(move |worker| {
            let result = engine.terminate(&target, kind);
            worker.send(ProcessesMsg::Quit {
                target,
                kind,
                result,
            });
        });
        match spawned {
            Ok(task) => self.quitting = Some((name, task.id())),
            Err(error) => {
                self.notice = Some(
                    StyledLine::new()
                        .bad("● ")
                        .plain(format!("Nothing was sent: {error}")),
                );
            }
        }
    }

    fn on_box_key(&mut self, key: &KeyEvent, ctx: &mut PageCtx<'_, ProcessesMsg>) {
        let Some(pending) = self.pending.as_mut() else {
            return;
        };
        match pending.confirm.on_key(key) {
            ConfirmReply::Open => {}
            ConfirmReply::Cancelled => self.pending = None,
            ConfirmReply::Confirmed => {
                if let Some(Pending { target, kind, .. }) = self.pending.take() {
                    self.quit(target, kind, ctx);
                }
            }
        }
    }

    fn foot(&self, tick: u64) -> StyledLine {
        if let Some((name, _)) = &self.quitting {
            return spinner_line(tick, format!("quitting {name}…"));
        }
        if let Some(notice) = &self.notice {
            return notice.clone();
        }
        if self.reading.is_some() {
            return spinner_line(tick, "reading again…");
        }
        StyledLine::new().dim(view::CPU_NOTE)
    }

    fn keys(&self) -> Vec<KeyHint> {
        if self.pending.is_some() {
            return ConfirmBox::key_hints();
        }
        if self.rows.is_none() {
            return KeyHint::globals();
        }
        [
            KeyHint::new("↑↓", "move"),
            KeyHint::new("s", "sort"),
            KeyHint::new("r", "read again"),
            KeyHint::new("x", "quit"),
            KeyHint::new("X", "force quit"),
        ]
        .into_iter()
        .chain(KeyHint::globals())
        .collect()
    }
}

/// The engine's own order (`sort_processes`), applied to whole rows.
fn sorted(rows: Vec<ProcessRow>, sort: ProcessSort) -> Vec<ProcessRow> {
    let mut infos: Vec<_> = rows.iter().map(|row| row.info.clone()).collect();
    processes::sort_processes(&mut infos, sort);
    let mut by_pid: HashMap<u32, ProcessRow> =
        rows.into_iter().map(|row| (row.info.pid, row)).collect();
    infos
        .iter()
        .filter_map(|info| by_pid.remove(&info.pid))
        .collect()
}

impl<E: ProcessEngine> Page for ProcessesPage<E> {
    const ID: PageId = PageId::Processes;
    type Msg = ProcessesMsg;

    fn view(&self, ctx: &ViewCtx) -> PageView {
        let keys = self.keys();
        let Some(rows) = &self.rows else {
            let list = match &self.failed {
                Some(reason) => vec![
                    StyledLine::new()
                        .bad("● ")
                        .hi("Processes could not be read"),
                    StyledLine::new().dim(format!("  {reason}. r reads again.")),
                ],
                None => vec![spinner_line(ctx.tick, "reading processes…")],
            };
            return PageView {
                list,
                keys,
                ..PageView::default()
            };
        };
        let cursor = self.cursor.index();
        let list = if rows.is_empty() {
            vec![StyledLine::new().dim("No process could be read.")]
        } else {
            rows.iter()
                .enumerate()
                .map(|(i, row)| view::row(&row.info, i == cursor, ctx.list_width))
                .collect()
        };
        let detail = rows.get(cursor).map(|row| {
            let refused = self
                .refused
                .as_ref()
                .filter(|(key, _)| *key == key_of(row))
                .map(|(_, reason)| reason);
            view::detail(row, refused, ctx.detail_width)
        });
        PageView {
            header: vec![view::heading()],
            cursor: (!rows.is_empty()).then_some(cursor),
            list,
            detail,
            foot: Some(self.foot(ctx.tick)),
            keys,
            overlay: self.pending.as_ref().map(|p| p.confirm.overlay()),
            ..PageView::default()
        }
    }

    fn on_show(&mut self, ctx: &mut PageCtx<'_, ProcessesMsg>) {
        if self.rows.is_none() && self.failed.is_none() {
            self.start_read(ctx);
        }
    }

    fn on_reload(&mut self, ctx: &mut PageCtx<'_, ProcessesMsg>) {
        self.start_read(ctx);
    }

    fn on_key(&mut self, key: KeyEvent, ctx: &mut PageCtx<'_, ProcessesMsg>) -> KeyReply {
        if self.pending.is_some() {
            self.on_box_key(&key, ctx);
            return KeyReply::Handled;
        }
        if key.code == KeyCode::Char('r') {
            self.notice = None;
            self.failed = None;
            self.start_read(ctx);
            return KeyReply::Handled;
        }
        let Some(len) = self.rows.as_ref().map(Vec::len) else {
            return KeyReply::Ignored;
        };
        if self.cursor.on_key(&key, len, ctx.list_rows(), |_| true) {
            return KeyReply::Handled;
        }
        match key.code {
            KeyCode::Char('s') => {
                self.sort = match self.sort {
                    ProcessSort::Cpu => ProcessSort::Memory,
                    ProcessSort::Memory => ProcessSort::Cpu,
                };
                let kept = self.highlighted().map(key_of);
                let rows = self.rows.take().unwrap_or_default();
                self.place(rows, kept);
            }
            KeyCode::Char('x') => self.ask(TerminateKind::Graceful),
            KeyCode::Char('X') => self.ask(TerminateKind::Force),
            _ => return KeyReply::Ignored,
        }
        KeyReply::Handled
    }

    fn on_message(&mut self, message: ProcessesMsg, ctx: &mut PageCtx<'_, ProcessesMsg>) {
        match message {
            ProcessesMsg::Read(rows) => {
                self.reading = None;
                self.failed = None;
                let kept = self.highlighted().map(key_of);
                self.place(rows, kept);
            }
            ProcessesMsg::Quit {
                target,
                kind,
                result,
            } => {
                self.quitting = None;
                self.refused = None;
                self.notice = Some(view::outcome_line(&target, kind, &result));
                self.start_read(ctx);
            }
        }
    }

    fn on_task_failed(&mut self, task: TaskId, reason: &str, _ctx: &mut PageCtx<'_, ProcessesMsg>) {
        let quitting = self.quitting.take_if(|(_, id)| *id == task);
        let reading = self.reading.take_if(|id| *id == task);
        if quitting.is_none() && reading.is_none() {
            return;
        }
        if self.rows.is_none() {
            self.failed = Some(reason.to_string());
            return;
        }
        let what = if quitting.is_some() {
            "The quit stopped"
        } else {
            "Reading again stopped"
        };
        self.notice = Some(
            StyledLine::new()
                .bad("● ")
                .plain(format!("{what}: {reason}")),
        );
    }

    fn is_busy(&self) -> bool {
        self.reading.is_some() || self.quitting.is_some()
    }

    fn on_quit_request(&mut self, _ctx: &mut PageCtx<'_, ProcessesMsg>) {
        self.pending = None;
    }
}

#[cfg(test)]
mod fake_engine;
#[cfg(test)]
mod tests;
