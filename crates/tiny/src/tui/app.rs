//! The session's state and the global keys (I21), free of any terminal so it
//! can be driven by tests.

use crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;

use super::frame::{compose, compose_too_small, paint, TabBar};
use super::icons::IconMode;
use super::layout::Geometry;
use super::pages::{AnyPage, FrameRequest, KeyHint, KeyReply, PageEnv, PageId, ViewCtx};
use super::styled_line::{text_width, StyledLine};
use super::theme::Theme;
use super::widgets::overlay_box::Overlay;
use super::work::{LoopEvent, SignalKind, Spawner};

/// Reads the free space of the startup volume; runs on a worker thread.
pub type FreeSpaceReader = fn() -> Option<u64>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Flow {
    Continue,
    /// Every page let go: leave normally.
    Exit,
    /// A second Ctrl-C or SIGTERM while a write was finishing: leave at once.
    Abandon,
}

/// What the session leaves behind once the alternate screen is gone.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct SessionEnd {
    /// Printed to stdout: every page's exit card, in tab order.
    pub exit_card: Vec<StyledLine>,
    /// Printed to stderr, one line each.
    pub notices: Vec<String>,
    /// The session was left at once while this step (`category`, `app`) was
    /// in progress: nothing else is printed and the status is 130 (S6).
    pub abandoned: Option<&'static str>,
}

pub struct App {
    pages: Vec<Box<dyn AnyPage>>,
    active: PageId,
    help_open: bool,
    offsets: [usize; PageId::ALL.len()],
    list_rows: [usize; PageId::ALL.len()],
    free_bytes: Option<u64>,
    free_space: FreeSpaceReader,
    icons: IconMode,
    quitting: bool,
    abandoned: Option<&'static str>,
    too_small: bool,
    tick: u64,
    spawner: Spawner,
    notices: Vec<String>,
}

impl App {
    /// `pages` must be in tab order (`pages::all_pages()`).
    pub fn new(
        pages: Vec<Box<dyn AnyPage>>,
        icons: IconMode,
        spawner: Spawner,
        free_space: FreeSpaceReader,
    ) -> Self {
        debug_assert!(pages.iter().map(|p| p.id()).eq(PageId::ALL));
        Self {
            pages,
            active: PageId::Clean,
            help_open: false,
            offsets: [0; PageId::ALL.len()],
            list_rows: [0; PageId::ALL.len()],
            free_bytes: None,
            free_space,
            icons,
            quitting: false,
            abandoned: None,
            too_small: false,
            tick: 0,
            spawner,
            notices: Vec::new(),
        }
    }

    /// Opens on Clean (TU12) and reads the free-disk figure.
    pub fn start(&mut self) {
        self.start_on(PageId::Clean);
    }

    /// Opens on `page`; tests open the page they drive.
    pub fn start_on(&mut self, page: PageId) {
        self.active = page;
        self.refresh_free_space();
        self.call(page, |p, env| p.on_show(env));
    }

    #[cfg(test)]
    pub fn active(&self) -> PageId {
        self.active
    }

    /// True while any page works: the loop then redraws on a timer.
    pub fn needs_ticks(&self) -> bool {
        self.pages.iter().any(|page| page.is_busy())
    }

    /// The spinner frame for the next draw.
    pub fn set_tick(&mut self, tick: u64) {
        self.tick = tick;
    }

    pub fn handle(&mut self, event: LoopEvent) -> Flow {
        self.apply(event);
        self.flow()
    }

    fn apply(&mut self, event: LoopEvent) {
        match event {
            LoopEvent::Terminal(Event::Key(key)) if key.kind != KeyEventKind::Release => {
                self.on_key(key);
            }
            LoopEvent::Terminal(_) => {}
            LoopEvent::InputLost(reason) => {
                self.terminal_lost(&reason);
            }
            LoopEvent::Page { page, message } => {
                self.call(page, |p, env| p.on_message(message, env));
            }
            LoopEvent::TaskFailed { page, task, reason } => {
                self.notices.push(format!("{}: {reason}", page.title()));
                self.call(page, |p, env| p.on_task_failed(task, &reason, env));
            }
            LoopEvent::FreeSpace(bytes) => self.free_bytes = bytes,
            LoopEvent::Signal(SignalKind::Hangup) => self.request_quit(false),
            LoopEvent::Signal(SignalKind::Terminate) => self.request_quit(true),
        }
    }

    fn flow(&self) -> Flow {
        if self.abandoned.is_some() {
            Flow::Abandon
        } else if self.quitting && self.holder().is_none() {
            Flow::Exit
        } else {
            Flow::Continue
        }
    }

    /// The terminal can no longer be drawn or read: quit as `q` does, so a
    /// write in progress still finishes before the process leaves (S5).
    pub fn terminal_lost(&mut self, reason: &str) -> Flow {
        self.notices
            .push(format!("the terminal went away: {reason}"));
        self.request_quit(false);
        self.flow()
    }

    /// The page holding the exit, if any.
    fn holder(&self) -> Option<&dyn AnyPage> {
        self.pages
            .iter()
            .find(|page| page.holds_exit())
            .map(|page| page.as_ref())
    }

    pub fn draw(&mut self, buf: &mut Buffer, area: Rect, theme: &Theme) {
        self.too_small = !Geometry::fits(area.width, area.height);
        if self.too_small {
            paint(
                buf,
                area,
                &compose_too_small(area.width, area.height),
                theme,
            );
            return;
        }
        let geo = Geometry::new(area.width, area.height);
        let ctx = ViewCtx {
            width: geo.width,
            height: geo.height,
            two_columns: geo.two_columns,
            list_width: geo.list_width,
            detail_width: geo.detail_width,
            icons: self.icons,
            tick: self.tick,
        };
        let index = self.active.index();
        let mut view = self.pages[index].view(&ctx);
        if let Some(step) = self
            .quitting
            .then(|| self.holder())
            .flatten()
            .map(|p| p.held_step())
        {
            view.foot = Some(StyledLine::new().warn("● ").dim(format!(
                "Quitting after the {step} in progress · ctrl-c again quits now"
            )));
            view.keys = vec![KeyHint::new("ctrl-c", "quit now")];
            view.overlay = None;
        }
        if self.help_open {
            view.overlay = Some(help_overlay(&view.keys));
            view.keys = vec![KeyHint::new("esc", "close")];
        }
        let bar = TabBar {
            active: self.active,
            icons: self.icons,
            free_bytes: self.free_bytes,
        };
        let composed = compose(&geo, &bar, &view, self.offsets[index]);
        self.offsets[index] = composed.offset;
        self.list_rows[index] = composed.list_rows;
        paint(buf, area, &composed, theme);
    }

    /// Every page's exit card and the notices gathered during the session.
    pub fn finish(self) -> SessionEnd {
        SessionEnd {
            exit_card: self
                .pages
                .iter()
                .filter_map(|page| page.exit_card())
                .flatten()
                .collect(),
            notices: self.notices,
            abandoned: self.abandoned,
        }
    }

    fn on_key(&mut self, key: KeyEvent) {
        let ctrl_c =
            key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL);
        if ctrl_c {
            return self.request_quit(true);
        }
        if self.quitting {
            return;
        }
        if self.too_small {
            if key.code == KeyCode::Char('q') {
                self.request_quit(false);
            }
            return;
        }
        if self.help_open {
            if matches!(key.code, KeyCode::Esc | KeyCode::Enter | KeyCode::Char('?')) {
                self.help_open = false;
            }
            return;
        }
        if self.call(self.active, |page, env| page.on_key(key, env)) == KeyReply::Handled {
            return;
        }
        let count = PageId::ALL.len();
        match key.code {
            KeyCode::Tab => self.switch_to(PageId::ALL[(self.active.index() + 1) % count]),
            KeyCode::BackTab => {
                self.switch_to(PageId::ALL[(self.active.index() + count - 1) % count]);
            }
            KeyCode::Char(digit @ '1'..='9') => {
                let index = digit as usize - '1' as usize;
                if let Some(&page) = PageId::ALL.get(index) {
                    self.switch_to(page);
                }
            }
            KeyCode::Char('q') => self.request_quit(false),
            KeyCode::Char('?') => self.help_open = true,
            _ => {}
        }
    }

    /// `q`, Ctrl-C, a signal or a lost terminal. A `forceful` request (Ctrl-C,
    /// SIGINT, SIGTERM) while an earlier one waits for a write leaves at once.
    fn request_quit(&mut self, forceful: bool) {
        if self.quitting {
            if forceful {
                self.abandoned = self.holder().map(|page| page.held_step());
            }
            return;
        }
        self.quitting = true;
        self.help_open = false;
        for page in PageId::ALL {
            self.call(page, |p, env| p.on_quit_request(env));
        }
    }

    /// Only one run at a time: while a page holds the exit, no page is switched to (S4).
    fn switch_to(&mut self, page: PageId) {
        if page == self.active || self.holder().is_some() {
            return;
        }
        self.active = page;
        self.help_open = false;
        self.call(page, |p, env| p.on_show(env));
    }

    /// Not generic, so a page that reloads itself cannot recurse the compiler.
    fn reload(&mut self, page: PageId) {
        self.call(page, |p, env| p.on_reload(env));
    }

    fn refresh_free_space(&mut self) {
        let read = self.free_space;
        // A hung volume must not stall the UI; the figure appears when read.
        let spawned = self
            .spawner
            .run_counted("sift-free-space".into(), move |tx| {
                let _ = tx.send(LoopEvent::FreeSpace(read()));
            });
        if spawned.is_err() {
            self.free_bytes = None;
        }
    }

    /// Calls into one page, then applies what it asked of the frame.
    fn call<R>(&mut self, page: PageId, f: impl FnOnce(&mut dyn AnyPage, &mut PageEnv) -> R) -> R {
        let mut env = PageEnv {
            page,
            spawner: self.spawner.clone(),
            requests: Vec::new(),
            list_rows: self.list_rows[page.index()],
        };
        let reply = f(self.pages[page.index()].as_mut(), &mut env);
        for request in env.requests {
            match request {
                FrameRequest::Show(target) => self.switch_to(target),
                FrameRequest::Reload(target) => self.reload(target),
                FrameRequest::RefreshFreeSpace => self.refresh_free_space(),
            }
        }
        reply
    }
}

/// The `?` box: the page's keys, then the global ones.
fn help_overlay(page_keys: &[KeyHint]) -> Overlay {
    let globals = [
        KeyHint::new("tab / shift-tab", "next or previous page"),
        KeyHint::new("1 to 5", "go to a page"),
        KeyHint::new("esc", "step back"),
        KeyHint::new("q / ctrl-c", "quit"),
        KeyHint::new("?", "this list"),
    ];
    // The frame's own `tab` and `q` are listed below as globals, and so is a
    // page hint that repeats a global row; `x quit` is the page's own key.
    let global = |hint: &KeyHint| {
        hint.key == "tab"
            || hint.key == "q"
            || globals
                .iter()
                .any(|g| g.key == hint.key && g.action == hint.action)
    };
    let own: Vec<&KeyHint> = page_keys.iter().filter(|hint| !global(hint)).collect();
    let key_width = own
        .iter()
        .copied()
        .chain(globals.iter())
        .map(|hint| text_width(&hint.key))
        .max()
        .unwrap_or(0)
        + 3;
    let row = |hint: &KeyHint| {
        let pad = key_width.saturating_sub(text_width(&hint.key));
        StyledLine::new()
            .hi(hint.key.as_str())
            .plain(" ".repeat(pad))
            .dim(hint.action.as_str())
    };
    let page_rows: Vec<StyledLine> = own.into_iter().map(row).collect();
    let spacer = (!page_rows.is_empty()).then(StyledLine::new);
    let lines = std::iter::once(StyledLine::new())
        .chain(page_rows)
        .chain(spacer)
        .chain(globals.iter().map(row))
        .chain(std::iter::once(StyledLine::new()))
        .collect();
    Overlay {
        title: "Keys".into(),
        lines,
    }
}

#[cfg(test)]
#[path = "app_tests.rs"]
mod tests;
