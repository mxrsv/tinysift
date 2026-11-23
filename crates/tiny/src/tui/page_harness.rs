//! Test harness: one page in the real frame, drawn by ratatui's `TestBackend`,
//! read back as plain text to compare with the mock's text dump.
//!
//! ```ignore
//! let mut screen = PageHarness::new(my_page(), STANDARD, IconMode::Unicode);
//! screen.press(KeyCode::Char('v')).settle();
//! assert_eq!(screen.text(), include_str!("fixtures/clean_review_std.txt").trim_end());
//! ```
//!
//! `text()` trims each line's trailing spaces, as
//! `node ~/scratch/tiny-cli/2026-11-21-sift-tui/tools/dump.js <screen>` prints
//! it, so a fixture is that dump without its `#####` header line.

use std::any::Any;
use std::sync::mpsc::{self, Receiver};
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::buffer::Buffer;
use ratatui::Terminal;
use unicode_width::UnicodeWidthStr;

use super::app::{App, Flow, SessionEnd};
use super::icons::IconMode;
use super::pages::{register, AnyPage, Page, PageId, PageView, ViewCtx};
use super::theme::Theme;
use super::work::{LoopEvent, Spawner};

/// Terminal.app's default size.
pub const STANDARD: (u16, u16) = (80, 24);
/// The mock's wide size.
pub const WIDE: (u16, u16) = (120, 34);
/// The free-disk figure in the mock's normal data: `28.8 GB free`.
pub const MOCK_FREE_BYTES: u64 = 28_800_000_000;
/// How long `settle` waits for background tasks.
const SETTLE_LIMIT: Duration = Duration::from_secs(10);

/// A page with no engine that draws nothing but the shared key hints.
macro_rules! blank_page {
    ($name:ident, $id:expr) => {
        struct $name;
        impl Page for $name {
            const ID: PageId = $id;
            type Msg = std::convert::Infallible;
            fn view(&self, _ctx: &ViewCtx) -> PageView {
                PageView {
                    keys: super::pages::KeyHint::globals(),
                    ..PageView::default()
                }
            }
        }
    };
}
blank_page!(BlankClean, PageId::Clean);
blank_page!(BlankUninstall, PageId::Uninstall);
blank_page!(BlankScan, PageId::Scan);
blank_page!(BlankProcesses, PageId::Processes);
blank_page!(BlankSystem, PageId::System);

/// Five blank pages in tab order.
fn blank_pages() -> Vec<Box<dyn AnyPage>> {
    vec![
        register(BlankClean),
        register(BlankUninstall),
        register(BlankScan),
        register(BlankProcesses),
        register(BlankSystem),
    ]
}

pub struct PageHarness {
    app: App,
    rx: Receiver<LoopEvent>,
    spawner: Spawner,
    terminal: Terminal<TestBackend>,
    theme: Theme,
    exited: bool,
}

impl PageHarness {
    /// `page` in its own tab, opened as the user would open it, with
    /// background work settled. The other four tabs are blank pages: a test
    /// never builds a real page, whose engine would read this machine.
    pub fn new<P: Page + 'static>(page: P, size: (u16, u16), icons: IconMode) -> Self {
        let mut pages = blank_pages();
        pages[P::ID.index()] = register(page);
        Self::with_pages(pages, P::ID, size, icons)
    }

    /// The frame alone: five blank pages, opened on Clean.
    pub fn blank(size: (u16, u16), icons: IconMode) -> Self {
        Self::with_pages(blank_pages(), PageId::Clean, size, icons)
    }

    /// Five pages of your own, in tab order (wrap each with `pages::register`).
    pub fn with_pages(
        pages: Vec<Box<dyn AnyPage>>,
        open: PageId,
        (width, height): (u16, u16),
        icons: IconMode,
    ) -> Self {
        let (tx, rx) = mpsc::channel();
        let spawner = Spawner::new(tx);
        let mut app = App::new(pages, icons, spawner.clone(), || Some(MOCK_FREE_BYTES));
        app.start_on(open);
        let mut harness = Self {
            app,
            rx,
            spawner,
            terminal: Terminal::new(TestBackend::new(width, height)).expect("test backend"),
            theme: Theme::coloured(),
            exited: false,
        };
        harness.settle();
        harness
    }

    /// Draw without colour, as with `NO_COLOR` set.
    pub fn monochrome(&mut self) -> &mut Self {
        self.theme = Theme::monochrome();
        self
    }

    pub fn press(&mut self, code: KeyCode) -> &mut Self {
        self.press_key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    pub fn press_key(&mut self, key: KeyEvent) -> &mut Self {
        self.handle(LoopEvent::Terminal(Event::Key(key)));
        self
    }

    /// Delivers `message` to `page` as if one of its tasks had sent it.
    pub fn deliver(&mut self, page: PageId, message: impl Any + Send) -> &mut Self {
        self.handle(LoopEvent::Page {
            page,
            message: Box::new(message),
        });
        self
    }

    /// A task of `page` panicked with `reason`, as its worker reports it.
    pub fn fail(&mut self, page: PageId, reason: &str) -> &mut Self {
        self.handle(LoopEvent::TaskFailed {
            page,
            reason: reason.into(),
        });
        self
    }

    /// SIGTERM, SIGHUP or SIGINT arrived.
    pub fn signal(&mut self) -> &mut Self {
        self.handle(LoopEvent::Signal);
        self
    }

    /// Handles queued events until no task runs and nothing is queued.
    pub fn settle(&mut self) -> &mut Self {
        let started = Instant::now();
        loop {
            while let Ok(event) = self.rx.try_recv() {
                self.handle(event);
            }
            if self.spawner.live_tasks() == 0 {
                // A task may have sent its last message just before ending.
                while let Ok(event) = self.rx.try_recv() {
                    self.handle(event);
                }
                return self;
            }
            assert!(
                started.elapsed() < SETTLE_LIMIT,
                "background work did not finish"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    /// The spinner frame the next draw uses.
    pub fn set_tick(&mut self, tick: u64) -> &mut Self {
        self.app.set_tick(tick);
        self
    }

    pub fn resize(&mut self, width: u16, height: u16) -> &mut Self {
        self.terminal.backend_mut().resize(width, height);
        self
    }

    /// The screen as text: one line per row, trailing spaces trimmed.
    pub fn text(&mut self) -> String {
        buffer_text(&self.buffer())
    }

    /// The drawn cells with their styles, for colour and selection checks.
    pub fn buffer(&mut self) -> Buffer {
        let (app, theme) = (&mut self.app, &self.theme);
        self.terminal
            .draw(|frame| {
                let area = frame.area();
                app.draw(frame.buffer_mut(), area, theme);
            })
            .expect("test backend draws");
        self.terminal.backend().buffer().clone()
    }

    pub fn active(&self) -> PageId {
        self.app.active()
    }

    /// True once `q`, Ctrl-C or a signal ended the session.
    pub fn exited(&self) -> bool {
        self.exited
    }

    pub fn finish(self) -> SessionEnd {
        self.app.finish()
    }

    fn handle(&mut self, event: LoopEvent) {
        if self.app.handle(event) == Flow::Exit {
            self.exited = true;
        }
    }
}

/// Rows of `buffer` as text, trailing spaces trimmed; a wide glyph's hidden
/// second cell is skipped.
pub fn buffer_text(buffer: &Buffer) -> String {
    let area = buffer.area;
    (area.top()..area.bottom())
        .map(|y| {
            let mut line = String::new();
            let mut x = area.left();
            while x < area.right() {
                let symbol = buffer[(x, y)].symbol();
                line.push_str(symbol);
                x += u16::try_from(symbol.width().max(1)).unwrap_or(1);
            }
            line.trim_end().to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}
