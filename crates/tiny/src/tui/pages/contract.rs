//! The contract every page implements. A page owns its data and its keys; the
//! frame owns the tab bar, the rules, the layout, scrolling and the key bar.
//!
//! A page describes one screen as a [`PageView`] each frame: list rows (with
//! the cursor row), the detail of the highlighted row in a full form (right
//! pane, 100 columns and up) and a three-line brief (band under the list,
//! below 100 columns), an optional foot line, the key hints, and an optional
//! box over the page. It never draws to the terminal and never blocks: slow
//! work runs through [`PageCtx::spawn`] and comes back through
//! [`Page::on_message`].

use crossterm::event::KeyEvent;

use crate::tui::icons::IconMode;
use crate::tui::styled_line::StyledLine;
use crate::tui::widgets::overlay_box::Overlay;
use crate::tui::work::{Spawner, TaskHandle, Worker};

/// The five tabs, in tab-bar order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PageId {
    Clean,
    Uninstall,
    Scan,
    Processes,
    System,
}

impl PageId {
    pub const ALL: [PageId; 5] = [
        PageId::Clean,
        PageId::Uninstall,
        PageId::Scan,
        PageId::Processes,
        PageId::System,
    ];

    pub fn title(self) -> &'static str {
        match self {
            Self::Clean => "Clean",
            Self::Uninstall => "Uninstall",
            Self::Scan => "Scan",
            Self::Processes => "Processes",
            Self::System => "System",
        }
    }

    /// Position in the tab bar, from 0.
    pub fn index(self) -> usize {
        Self::ALL.iter().position(|&id| id == self).unwrap_or(0)
    }
}

/// One entry of the key bar: `↑↓ move`, `space tick`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeyHint {
    pub key: String,
    pub action: String,
}

impl KeyHint {
    pub fn new(key: impl Into<String>, action: impl Into<String>) -> Self {
        Self {
            key: key.into(),
            action: action.into(),
        }
    }

    /// `tab page · q quit`, which ends the bar of every page at rest.
    pub fn globals() -> Vec<KeyHint> {
        vec![KeyHint::new("tab", "page"), KeyHint::new("q", "quit")]
    }
}

/// The highlighted row's detail.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Detail {
    /// Dim title: right pane's first line, or the band's rule (`── Highlighted ──`).
    pub title: String,
    /// The right-hand pane (`ViewCtx::detail_width` wide), under the title and a blank line.
    pub full: Vec<StyledLine>,
    /// Three lines for the band below 100 columns (`ViewCtx::list_width` wide).
    /// Empty: the first three lines of `full` are used.
    pub brief: Vec<StyledLine>,
}

/// One frame of a page.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageView {
    /// Rows pinned above the list (column headings, view switch); never scroll.
    pub header: Vec<StyledLine>,
    /// The rows; the frame windows them and adds `… N more ↓`. Rows are clipped
    /// with `…` or padded to `ViewCtx::list_width`, so draw them at that width.
    pub list: Vec<StyledLine>,
    /// Index into `list` of the highlighted row; the frame keeps it in view
    /// and draws it reversed. The row itself starts with `❯ ` (the page draws it).
    pub cursor: Option<usize>,
    pub detail: Option<Detail>,
    /// The pinned line above the bottom rule, e.g. `6.3 GB picked in 3 categories`.
    pub foot: Option<StyledLine>,
    /// The whole key bar, in order. End with `KeyHint::globals()` at rest;
    /// a box or a run shows only its own keys.
    pub keys: Vec<KeyHint>,
    /// A centred box over the page (action menu, confirmation).
    pub overlay: Option<Overlay>,
}

/// What a page may know while it builds a view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewCtx {
    pub width: usize,
    pub height: usize,
    /// True from 100 columns: detail on the right, else a band under the list.
    pub two_columns: bool,
    /// Cells in a list row.
    pub list_width: usize,
    /// Cells in a line of the right pane (two columns) or of the band.
    pub detail_width: usize,
    pub icons: IconMode,
    /// Frame counter for [`spinner_glyph`](crate::tui::widgets::spinner::spinner_glyph);
    /// it advances only while a page reports [`Page::is_busy`].
    pub tick: u64,
}

/// What a key did.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum KeyReply {
    /// The page used the key; the frame does nothing more with it.
    Handled,
    /// Not the page's: the frame applies the global keys (tab, shift-tab,
    /// digits, `q`, `?`). Return `Handled` for every key while a box is open.
    Ignored,
}

/// A page. Implement it in `pages/<name>.rs` (or `pages/<name>/mod.rs`) and
/// return it from that module's `page()`; the registry in `pages/mod.rs`
/// already calls it. Every method runs on the UI thread and must return fast.
pub trait Page {
    /// The tab this page fills.
    const ID: PageId;
    /// What this page's background tasks send back.
    type Msg: Send + 'static;

    /// The screen for this frame. Pure: no I/O, no state change.
    fn view(&self, ctx: &ViewCtx) -> PageView;

    /// The tab became active (also once at start for Clean, the first tab).
    /// Start a read only if the page reads on open; Scan waits for enter (I17).
    fn on_show(&mut self, _ctx: &mut PageCtx<'_, Self::Msg>) {}

    /// Another page (or this one) asked for a fresh read through [`PageCtx::reload`].
    fn on_reload(&mut self, _ctx: &mut PageCtx<'_, Self::Msg>) {}

    /// A key press, before the global keys. See [`KeyReply`].
    fn on_key(&mut self, _key: KeyEvent, _ctx: &mut PageCtx<'_, Self::Msg>) -> KeyReply {
        KeyReply::Ignored
    }

    /// A message from one of this page's tasks, delivered even while another tab is shown.
    fn on_message(&mut self, _message: Self::Msg, _ctx: &mut PageCtx<'_, Self::Msg>) {}

    /// One of this page's tasks panicked; `reason` is also printed after exit.
    /// Stop any spinner it drove and show the failure.
    fn on_task_failed(&mut self, _reason: &str, _ctx: &mut PageCtx<'_, Self::Msg>) {}

    /// True while work runs: the frame redraws on a timer so spinners turn.
    fn is_busy(&self) -> bool {
        false
    }

    /// `q`, Ctrl-C or SIGTERM. Close any open box without acting and cancel
    /// tasks; a step that changes files finishes first (I14).
    fn on_quit_request(&mut self, _ctx: &mut PageCtx<'_, Self::Msg>) {}

    /// True while a write must finish before the TUI may close.
    fn holds_exit(&self) -> bool {
        false
    }

    /// Lines printed to stdout after the TUI closes (the Clean result card, I24).
    fn exit_card(&self) -> Option<Vec<StyledLine>> {
        None
    }
}

/// Requests a page makes of the frame; applied after the page returns.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FrameRequest {
    Show(PageId),
    Reload(PageId),
    RefreshFreeSpace,
}

/// Untyped state behind every [`PageCtx`].
pub struct PageEnv {
    pub page: PageId,
    pub spawner: Spawner,
    pub requests: Vec<FrameRequest>,
    pub list_rows: usize,
    pub icons: IconMode,
}

/// The page's handle on the frame during one call.
pub struct PageCtx<'a, M> {
    env: &'a mut PageEnv,
    _message: std::marker::PhantomData<fn(M)>,
}

impl<'a, M: Send + 'static> PageCtx<'a, M> {
    pub fn new(env: &'a mut PageEnv) -> Self {
        Self {
            env,
            _message: std::marker::PhantomData,
        }
    }

    /// Runs `job` off the UI thread; its `Worker::send` messages arrive in
    /// [`Page::on_message`]. Keep the handle to cancel it. Spawning fails
    /// only when the OS refuses a thread; show that as the page's error.
    pub fn spawn<F>(&mut self, job: F) -> std::io::Result<TaskHandle>
    where
        F: FnOnce(Worker<M>) + Send + 'static,
    {
        self.env.spawner.spawn(self.env.page, job)
    }

    /// Visible list rows in the last frame, for PgUp/PgDn.
    pub fn list_rows(&self) -> usize {
        self.env.list_rows
    }

    #[cfg_attr(
        not(test),
        expect(dead_code, reason = "used by category rows in Clean, plan task 07")
    )]
    pub fn icons(&self) -> IconMode {
        self.env.icons
    }

    /// Switch to `page` (it gets `on_show`).
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "used to return to Clean after Uninstall, plan task 09"
        )
    )]
    pub fn show(&mut self, page: PageId) {
        self.env.requests.push(FrameRequest::Show(page));
    }

    /// Switch to `page` and ask it to read again: Done's enter returns to a
    /// fresh Clean (I15), a quit re-reads Processes (I18).
    #[cfg_attr(
        not(test),
        expect(
            dead_code,
            reason = "used by Clean's Done screen and Processes, plan tasks 05 and 07"
        )
    )]
    pub fn reload(&mut self, page: PageId) {
        self.env.requests.push(FrameRequest::Show(page));
        self.env.requests.push(FrameRequest::Reload(page));
    }

    /// Read the free-disk figure in the tab bar again (after a clean).
    pub fn refresh_free_space(&mut self) {
        self.env.requests.push(FrameRequest::RefreshFreeSpace);
    }
}
