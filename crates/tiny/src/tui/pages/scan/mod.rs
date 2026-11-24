//! Scan: large and old files in Downloads, Desktop and Documents. Opening the
//! tab reads nothing; enter starts the scan (I17). The only action is `o`,
//! show the highlighted file in Finder. The engine's walk cannot be
//! interrupted: a stopped walk runs to its end, and a new scan asked for
//! meanwhile waits for it instead of walking beside it (C5).

mod engine;
mod view;

use std::path::PathBuf;
use std::sync::Arc;

use crossterm::event::{KeyCode, KeyEvent};
use tiny_core::options::ScanSort;
use tiny_core::scan::sort::sort_files;
use tiny_core::scan::types::{FileEntry, ScanData};

use super::{KeyHint, KeyReply, Page, PageCtx, PageId, PageView, ViewCtx};
use crate::tui::styled_line::StyledLine;
use crate::tui::widgets::list_cursor::ListCursor;
use crate::tui::work::{TaskHandle, TaskId};
use engine::{RealScan, ScanEngine};
use view::Which;

pub enum ScanMsg {
    Progress {
        run: u64,
        text: String,
    },
    Finished {
        run: u64,
        outcome: Result<Found, String>,
    },
    Revealed(Result<(), String>),
}

/// A finished scan's two lists, largest first as `sift scan` sorts them.
pub struct Found {
    roots: Vec<PathBuf>,
    large: Vec<FileEntry>,
    old: Vec<FileEntry>,
}

impl Found {
    fn from_scan(data: ScanData) -> Self {
        let ScanData {
            roots,
            mut large,
            mut old,
            ..
        } = data;
        sort_files(&mut large, ScanSort::Size);
        sort_files(&mut old, ScanSort::Size);
        Self { roots, large, old }
    }
}

/// What the page shows when no scan runs.
enum Shown {
    Idle,
    Found(Found),
    Failed(String),
}

struct Running {
    run: u64,
    handle: TaskHandle,
    text: Option<String>,
}

/// A stopped walk that has not ended yet.
struct Stopping {
    run: u64,
    task: TaskId,
}

pub struct ScanPage<E> {
    engine: Arc<E>,
    home: Option<PathBuf>,
    shown: Shown,
    running: Option<Running>,
    stopping: Option<Stopping>,
    /// A scan asked for while a stopped walk was still ending: it starts then.
    queued: bool,
    /// Counts scans so a stopped one's late messages are ignored.
    runs: u64,
    which: Which,
    cursors: [ListCursor; 2],
    /// The `open -R` in flight.
    revealing: Option<TaskId>,
    reveal_error: Option<String>,
}

pub fn page() -> ScanPage<RealScan> {
    ScanPage::new(RealScan)
}

impl<E: ScanEngine> ScanPage<E> {
    fn new(engine: E) -> Self {
        Self {
            home: engine.home(),
            engine: Arc::new(engine),
            shown: Shown::Idle,
            running: None,
            stopping: None,
            queued: false,
            runs: 0,
            which: Which::Large,
            cursors: [ListCursor::default(); 2],
            revealing: None,
            reveal_error: None,
        }
    }

    fn files(&self) -> &[FileEntry] {
        match (&self.shown, self.which) {
            (Shown::Found(found), Which::Large) => &found.large,
            (Shown::Found(found), Which::Old) => &found.old,
            _ => &[],
        }
    }

    fn cursor(&self) -> &ListCursor {
        &self.cursors[self.which as usize]
    }

    fn start(&mut self, ctx: &mut PageCtx<'_, ScanMsg>) {
        if self.running.is_some() {
            return;
        }
        if self.stopping.is_some() {
            self.queued = true;
            return;
        }
        self.runs += 1;
        let run = self.runs;
        let engine = Arc::clone(&self.engine);
        let spawned = ctx.spawn(move |worker| {
            let progress = |text: String| {
                worker.send(ScanMsg::Progress { run, text });
            };
            let outcome = engine.scan(&progress).map(Found::from_scan);
            // Sent even when stopped: the page waits for this walk's end.
            worker.send(ScanMsg::Finished { run, outcome });
        });
        match spawned {
            Ok(handle) => {
                self.running = Some(Running {
                    run,
                    handle,
                    text: None,
                });
            }
            Err(error) => self.shown = Shown::Failed(error.to_string()),
        }
    }

    /// The engine's walk cannot be interrupted: its result is dropped when it
    /// ends, and no new walk starts before then.
    fn stop(&mut self) {
        self.queued = false;
        if let Some(running) = self.running.take() {
            running.handle.cancel();
            self.stopping = Some(Stopping {
                run: running.run,
                task: running.handle.id(),
            });
        }
    }

    /// The stopped walk ended: start the scan that waited for it.
    fn stopped_walk_ended(&mut self, ctx: &mut PageCtx<'_, ScanMsg>) {
        self.stopping = None;
        if std::mem::take(&mut self.queued) {
            self.start(ctx);
        }
    }

    fn reveal(&mut self, ctx: &mut PageCtx<'_, ScanMsg>) {
        let Some(file) = self.files().get(self.cursor().index()) else {
            return;
        };
        let path = file.path.clone();
        let engine = Arc::clone(&self.engine);
        self.reveal_error = None;
        match ctx.spawn(move |worker| {
            worker.send(ScanMsg::Revealed(engine.reveal(&path)));
        }) {
            Ok(task) => self.revealing = Some(task.id()),
            Err(error) => self.reveal_error = Some(error.to_string()),
        }
    }

    fn keys(&self) -> Vec<KeyHint> {
        let own = if self.running.is_some() || self.queued {
            vec![KeyHint::new("esc", "stop")]
        } else {
            match &self.shown {
                Shown::Idle => vec![KeyHint::new("enter", "scan")],
                Shown::Found(found) if !found.large.is_empty() || !found.old.is_empty() => vec![
                    KeyHint::new("↑↓", "move"),
                    KeyHint::new("v", "large/old"),
                    KeyHint::new("o", "show in Finder"),
                    KeyHint::new("r", "scan again"),
                ],
                _ => vec![KeyHint::new("r", "scan again")],
            }
        };
        own.into_iter().chain(KeyHint::globals()).collect()
    }

    /// The thresholds line, a blank, then `lines`: every state but a list.
    fn message(&self, lines: Vec<StyledLine>) -> PageView {
        PageView {
            list: [
                view::thresholds(self.which, view::DEFAULT_FOLDERS),
                StyledLine::new(),
            ]
            .into_iter()
            .chain(lines)
            .collect(),
            keys: self.keys(),
            ..PageView::default()
        }
    }

    fn found_view(&self, found: &Found, ctx: &ViewCtx) -> PageView {
        if found.large.is_empty() && found.old.is_empty() {
            return self.message(view::nothing_found());
        }
        let home = self.home.as_deref();
        let files = self.files();
        let cursor = self.cursor().index();
        // Only rows that can be in the window are drawn; a list can hold a million files.
        let near = |i: usize| i.abs_diff(cursor) <= ctx.height;
        let list = if files.is_empty() {
            vec![view::nothing_in(self.which)]
        } else {
            files
                .iter()
                .enumerate()
                .map(|(i, file)| {
                    if near(i) {
                        view::row(file, home, i == cursor, ctx.list_width)
                    } else {
                        StyledLine::new()
                    }
                })
                .collect()
        };
        PageView {
            header: vec![
                view::switch(self.which, found.large.len(), found.old.len()),
                view::thresholds(self.which, &view::folders(&found.roots)),
                StyledLine::new(),
            ],
            cursor: (!files.is_empty()).then_some(cursor),
            detail: files.get(cursor).map(|file| {
                view::detail(file, home, self.reveal_error.as_deref(), ctx.detail_width)
            }),
            list,
            keys: self.keys(),
            ..PageView::default()
        }
    }

    fn on_found_key(&mut self, key: &KeyEvent, ctx: &mut PageCtx<'_, ScanMsg>) -> KeyReply {
        let len = self.files().len();
        let rows = ctx.list_rows();
        if self.cursors[self.which as usize].on_key(key, len, rows, |_| true) {
            self.reveal_error = None;
            return KeyReply::Handled;
        }
        match key.code {
            KeyCode::Char('v') => {
                self.which = match self.which {
                    Which::Large => Which::Old,
                    Which::Old => Which::Large,
                };
                self.reveal_error = None;
            }
            KeyCode::Char('o') => self.reveal(ctx),
            _ => return KeyReply::Ignored,
        }
        KeyReply::Handled
    }
}

impl<E: ScanEngine> Page for ScanPage<E> {
    const ID: PageId = PageId::Scan;
    type Msg = ScanMsg;

    fn view(&self, ctx: &ViewCtx) -> PageView {
        if let Some(running) = &self.running {
            return self.message(vec![view::scanning(running.text.as_deref(), ctx.tick)]);
        }
        if self.queued {
            return self.message(vec![view::waiting_for_stop(ctx.tick)]);
        }
        match &self.shown {
            Shown::Idle => self.message(vec![view::idle()]),
            Shown::Failed(error) => self.message(view::failed(
                &view::shown_error(error, self.home.as_deref()),
                ctx.list_width,
            )),
            Shown::Found(found) => self.found_view(found, ctx),
        }
    }

    fn on_key(&mut self, key: KeyEvent, ctx: &mut PageCtx<'_, ScanMsg>) -> KeyReply {
        if self.running.is_some() || self.queued {
            if key.code != KeyCode::Esc {
                return KeyReply::Ignored;
            }
            self.stop();
            return KeyReply::Handled;
        }
        match (&self.shown, key.code) {
            (Shown::Idle, KeyCode::Enter) | (_, KeyCode::Char('r')) => {
                self.start(ctx);
                KeyReply::Handled
            }
            (Shown::Found(_), _) => self.on_found_key(&key, ctx),
            _ => KeyReply::Ignored,
        }
    }

    fn on_message(&mut self, message: ScanMsg, ctx: &mut PageCtx<'_, ScanMsg>) {
        match message {
            ScanMsg::Progress { run, text } => {
                if let Some(running) = self.running.as_mut().filter(|r| r.run == run) {
                    running.text = Some(text);
                }
            }
            ScanMsg::Finished { run, outcome } => {
                if self.stopping.as_ref().is_some_and(|s| s.run == run) {
                    return self.stopped_walk_ended(ctx);
                }
                if self.running.as_ref().is_none_or(|r| r.run != run) {
                    return;
                }
                self.running = None;
                self.reveal_error = None;
                self.cursors = [ListCursor::default(); 2];
                // A failed root shows its error and no partial list, as the command does.
                self.shown = match outcome {
                    Ok(found) => Shown::Found(found),
                    Err(error) => Shown::Failed(error),
                };
            }
            ScanMsg::Revealed(result) => {
                self.revealing = None;
                self.reveal_error = result.err();
            }
        }
    }

    fn on_task_failed(&mut self, task: TaskId, reason: &str, ctx: &mut PageCtx<'_, ScanMsg>) {
        if self.running.as_ref().is_some_and(|r| r.handle.id() == task) {
            self.running = None;
            self.shown = Shown::Failed(reason.to_string());
        } else if self.stopping.as_ref().is_some_and(|s| s.task == task) {
            // A stopped scan's failure is not shown: its result was already dropped.
            self.stopped_walk_ended(ctx);
        } else if self.revealing.take_if(|id| *id == task).is_some() {
            self.reveal_error = Some(reason.to_string());
        }
    }

    fn is_busy(&self) -> bool {
        self.running.is_some() || self.queued
    }

    fn on_quit_request(&mut self, _ctx: &mut PageCtx<'_, ScanMsg>) {
        self.stop();
    }
}

#[cfg(test)]
mod tests;
