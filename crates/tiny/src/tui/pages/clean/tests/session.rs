//! A whole session as `main` runs it, short of the real terminal: the event
//! loop, the terminal guard and the printing after it. The real binary has
//! no hook to take a fake engine, so this is as close to `main` as a clean
//! can be tested without touching files (C9). Also what happens when the
//! terminal or the UI thread fails during a run (S5).

use std::io::{self, Write};
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
use ratatui::buffer::Cell;
use ratatui::layout::{Position, Size};
use ratatui::Terminal;

use super::super::fake_data::normal;
use super::super::fake_engine::FakeEngine;
use super::super::CleanPage;
use crate::tui::app::App;
use crate::tui::entry::report;
use crate::tui::event_loop::{drive, run_session};
use crate::tui::icons::IconMode;
use crate::tui::page_harness::blank_pages;
use crate::tui::pages::{register, Page, PageCtx, PageId, PageView, ViewCtx};
use crate::tui::terminal::{ScreenGuard, TtyControl};
use crate::tui::theme::Theme;
use crate::tui::work::{LoopEvent, Spawner};

type Log = Arc<Mutex<Vec<String>>>;

/// The terminal's mode switches, written to the shared log.
struct LoggedTty(Log);

impl TtyControl for LoggedTty {
    fn enter(&mut self) -> io::Result<()> {
        self.0.lock().unwrap().push("enter".into());
        Ok(())
    }

    fn leave(&mut self) -> io::Result<()> {
        self.0.lock().unwrap().push("leave".into());
        Ok(())
    }
}

/// Stdout: what is printed, and a mark in the shared log when it is.
struct LoggedOut(Log, Vec<u8>);

impl Write for LoggedOut {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if self.1.is_empty() {
            self.0.lock().unwrap().push("printed".into());
        }
        self.1.extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// A terminal that stops taking frames once `broken` is set, as a closed tab does.
struct Breaking {
    inner: TestBackend,
    broken: Arc<AtomicBool>,
}

fn never(error: std::convert::Infallible) -> io::Error {
    match error {}
}

impl Backend for Breaking {
    type Error = io::Error;

    fn draw<'a, I>(&mut self, content: I) -> io::Result<()>
    where
        I: Iterator<Item = (u16, u16, &'a Cell)>,
    {
        if self.broken.load(Ordering::SeqCst) {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        self.inner.draw(content).map_err(never)
    }
    fn hide_cursor(&mut self) -> io::Result<()> {
        self.inner.hide_cursor().map_err(never)
    }
    fn show_cursor(&mut self) -> io::Result<()> {
        self.inner.show_cursor().map_err(never)
    }
    fn get_cursor_position(&mut self) -> io::Result<Position> {
        self.inner.get_cursor_position().map_err(never)
    }
    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> io::Result<()> {
        self.inner.set_cursor_position(position).map_err(never)
    }
    fn clear(&mut self) -> io::Result<()> {
        self.inner.clear().map_err(never)
    }
    fn clear_region(&mut self, clear_type: ClearType) -> io::Result<()> {
        self.inner.clear_region(clear_type).map_err(never)
    }
    fn size(&self) -> io::Result<Size> {
        self.inner.size().map_err(never)
    }
    fn window_size(&mut self) -> io::Result<WindowSize> {
        self.inner.window_size().map_err(never)
    }
    fn flush(&mut self) -> io::Result<()> {
        if self.broken.load(Ordering::SeqCst) {
            return Err(io::Error::from_raw_os_error(libc::EIO));
        }
        self.inner.flush().map_err(never)
    }
}

/// A page that panics on its first message: the UI thread failing.
struct Bomb;

impl Page for Bomb {
    const ID: PageId = PageId::Uninstall;
    type Msg = ();

    fn view(&self, _ctx: &ViewCtx) -> PageView {
        PageView::default()
    }

    fn on_message(&mut self, _message: (), _ctx: &mut PageCtx<'_, ()>) {
        panic!("a page broke while a clean ran");
    }
}

struct Setup {
    app: App,
    rx: Receiver<LoopEvent>,
    tx: mpsc::Sender<LoopEvent>,
    spawner: Spawner,
    engine: Arc<FakeEngine>,
}

/// The Clean page on the fake engine in a real `App`, opened and measured.
fn setup(bomb: bool) -> Setup {
    let engine = Arc::new(FakeEngine::with(normal()));
    let mut pages = blank_pages();
    pages[PageId::Clean.index()] = register(CleanPage::new(engine.clone(), Arc::new(Instant::now)));
    if bomb {
        pages[PageId::Uninstall.index()] = register(Bomb);
    }
    let (tx, rx) = mpsc::channel();
    let spawner = Spawner::new(tx.clone());
    let mut app = App::new(pages, IconMode::Unicode, spawner.clone(), || None);
    app.start();
    let mut setup = Setup {
        app,
        rx,
        tx,
        spawner,
        engine,
    };
    setup.pump();
    setup
}

impl Setup {
    /// Handles events until no task runs and nothing is queued.
    fn pump(&mut self) {
        let started = Instant::now();
        while self.spawner.live_tasks() > 0 || self.handle_queued() {
            assert!(started.elapsed() < Duration::from_secs(10));
            std::thread::sleep(Duration::from_millis(2));
        }
        self.handle_queued();
    }

    fn handle_queued(&mut self) -> bool {
        let events: Vec<LoopEvent> = self.rx.try_iter().collect();
        let any = !events.is_empty();
        for event in events {
            self.app.handle(event);
        }
        any
    }

    fn key(&self, code: KeyCode) {
        let key = KeyEvent::new(code, KeyModifiers::NONE);
        self.tx.send(LoopEvent::Terminal(Event::Key(key))).unwrap();
    }
}

/// Lets the held engine go after `delay`, from another thread.
fn release_later(engine: &Arc<FakeEngine>, delay: Duration) {
    let engine = engine.clone();
    std::thread::spawn(move || {
        std::thread::sleep(delay);
        engine.release();
    });
}

const HELD: Duration = Duration::from_millis(300);

/// C9: enter, Move to Trash, the run, `q`; the card is printed after the
/// terminal is given back, and the status is 0.
#[test]
fn a_scripted_session_prints_the_card_after_the_terminal_is_given_back() {
    let setup = setup(false);
    let log = Log::default();
    let guard = ScreenGuard::enter(LoggedTty(log.clone())).unwrap();
    let terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    setup.key(KeyCode::Enter);
    setup.key(KeyCode::Enter);
    let (engine, spawner, tx) = (
        setup.engine.clone(),
        setup.spawner.clone(),
        setup.tx.clone(),
    );
    std::thread::spawn(move || {
        // Quit once the clean has run to its end.
        while engine.plans.lock().unwrap().is_empty() || spawner.writes_in_progress() > 0 {
            std::thread::sleep(Duration::from_millis(5));
        }
        let q = KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE);
        tx.send(LoopEvent::Terminal(Event::Key(q))).unwrap();
    });
    let end = run_session(
        guard,
        terminal,
        setup.app,
        setup.rx,
        &setup.spawner,
        &Theme::coloured(),
    )
    .unwrap();
    let mut out = LoggedOut(log.clone(), Vec::new());
    let status = report(&end, &[], &mut out, &mut io::sink(), &Theme::monochrome()).unwrap();
    assert_eq!(status, 0);
    assert_eq!(*log.lock().unwrap(), ["enter", "leave", "printed"]);
    let printed = String::from_utf8(out.1).unwrap();
    assert!(printed.contains("TinySift"), "{printed}");
    assert!(printed.contains("Cleaned 6.3 GB"), "{printed}");
    assert!(printed.contains("In Trash 6.3 GB"), "{printed}");
}

/// S5: the terminal stops taking frames during a run (its tab was closed):
/// the loop stops drawing, the category in progress finishes, the run
/// stops after it, and only then does the loop return the error.
#[test]
fn a_dead_terminal_waits_for_the_category_in_progress() {
    let mut setup = setup(false);
    setup.engine.hold();
    setup.key(KeyCode::Enter);
    setup.key(KeyCode::Enter);
    setup.handle_queued();
    let broken = Arc::new(AtomicBool::new(true));
    let backend = Breaking {
        inner: TestBackend::new(80, 24),
        broken,
    };
    let mut terminal = Terminal::new(backend).unwrap();
    release_later(&setup.engine, HELD);
    let started = Instant::now();
    let outcome = drive(&mut terminal, &mut setup.app, &setup.rx, &Theme::coloured());
    assert!(started.elapsed() >= HELD, "it waited for the run");
    let error = format!("{:#}", outcome.unwrap_err());
    assert!(error.contains("cannot draw"), "{error}");
    assert_eq!(*setup.engine.ran.lock().unwrap(), ["xcode-derived"]);
    let end = setup.app.finish();
    let card: String = end.exit_card.iter().map(|l| l.text()).collect();
    assert!(card.contains("Cleaned 6.2 GB"), "{card}");
    assert!(
        end.notices[0].starts_with("the terminal went away"),
        "{:?}",
        end.notices
    );
}

/// S5: the UI thread panics during a run: nothing listens any more, so the
/// run stops after the category in progress, and the session waits for it
/// before the panic goes on.
#[test]
fn a_panic_on_the_ui_thread_waits_for_the_category_in_progress() {
    let mut setup = setup(true);
    setup.engine.hold();
    setup.key(KeyCode::Enter);
    setup.key(KeyCode::Enter);
    setup.handle_queued();
    setup
        .tx
        .send(LoopEvent::Page {
            page: PageId::Uninstall,
            message: Box::new(()),
        })
        .unwrap();
    let guard = ScreenGuard::enter(LoggedTty(Log::default())).unwrap();
    let terminal = Terminal::new(TestBackend::new(80, 24)).unwrap();
    release_later(&setup.engine, HELD);
    let started = Instant::now();
    let (engine, spawner) = (setup.engine.clone(), setup.spawner.clone());
    let outcome = catch_unwind(AssertUnwindSafe(move || {
        run_session(
            guard,
            terminal,
            setup.app,
            setup.rx,
            &setup.spawner,
            &Theme::coloured(),
        )
    }));
    assert!(outcome.is_err(), "the panic goes on");
    assert!(started.elapsed() >= HELD, "it waited for the run");
    assert_eq!(spawner.writes_in_progress(), 0);
    assert_eq!(
        *engine.ran.lock().unwrap(),
        ["xcode-derived"],
        "stopped after it"
    );
}
