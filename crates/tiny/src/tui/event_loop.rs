//! The session: enter the terminal, draw, wait for one event, handle it,
//! repeat; leave the terminal before anything is printed.
//!
//! The loop sleeps until something happens: a key, a resize, a page's
//! message, a signal. Only while a page is busy does it also wake on a timer,
//! so the spinner turns; at rest nothing moves (FE8).
//!
//! A write in progress (a clean, an uninstall) always reaches the end of its
//! step before the process leaves: when the terminal can no longer be drawn
//! or read, the loop stops drawing and keeps handling events until no page
//! holds the exit; when the UI thread panics, the loop stops listening, so
//! the write ends after its step, and waits for it (S5).

use std::panic::{catch_unwind, resume_unwind, AssertUnwindSafe};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::Terminal;

use super::app::{App, Flow, SessionEnd};
use super::icons::IconMode;
use super::pages::all_pages;
use super::terminal::{forward_signals, install_panic_hook, RealTty, ScreenGuard, TtyControl};
use super::theme::Theme;
use super::work::{LoopEvent, Spawner};

/// One spinner frame.
const TICK: Duration = Duration::from_millis(100);
/// How often a failed session looks again at the write it waits for.
const WRITE_POLL: Duration = Duration::from_millis(20);

/// Runs a whole session in the real terminal and returns what to print after it.
pub fn run(icons: IconMode, theme: Theme) -> Result<SessionEnd> {
    let (tx, rx) = mpsc::channel();
    forward_signals(tx.clone()).context("cannot listen for signals")?;
    install_panic_hook();
    let guard = ScreenGuard::enter(RealTty).context("cannot enter the full-screen view")?;
    let terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    read_terminal_events(tx.clone())?;
    let spawner = Spawner::new(tx);
    let mut app = App::new(all_pages(), icons, spawner.clone(), read_free_space);
    app.start();
    run_session(guard, terminal, app, rx, &spawner, &theme)
}

/// Drives a started `app` until it ends, then gives the terminal back. A
/// panic on the UI thread waits for the write in progress, then goes on.
pub fn run_session<T, B>(
    guard: ScreenGuard<T>,
    mut terminal: Terminal<B>,
    mut app: App,
    rx: Receiver<LoopEvent>,
    spawner: &Spawner,
    theme: &Theme,
) -> Result<SessionEnd>
where
    T: TtyControl,
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        drive(&mut terminal, &mut app, &rx, theme)
    }));
    drop(terminal);
    let outcome = match outcome {
        Ok(outcome) => outcome,
        Err(panic) => {
            // Nothing hears a task now, so a write stops after its step.
            drop(rx);
            wait_for_writes(spawner);
            drop(guard);
            resume_unwind(panic);
        }
    };
    guard.leave().context("cannot restore the terminal")?;
    outcome?;
    Ok(app.finish())
}

/// Draws and handles events until the app says to exit or every sender is
/// gone. A failed draw stops drawing and quits as `q` does: the loop goes on
/// until no page holds the exit, then returns the draw's error.
pub fn drive<B>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    rx: &Receiver<LoopEvent>,
    theme: &Theme,
) -> Result<()>
where
    B: Backend,
    B::Error: std::error::Error + Send + Sync + 'static,
{
    let started = Instant::now();
    let mut lost: Option<anyhow::Error> = None;
    let ended = |lost: Option<anyhow::Error>| lost.map_or(Ok(()), Err);
    loop {
        if lost.is_none() {
            app.set_tick((started.elapsed().as_millis() / TICK.as_millis()) as u64);
            let drawn = terminal.draw(|frame| {
                let area = frame.area();
                app.draw(frame.buffer_mut(), area, theme);
            });
            if let Err(error) = drawn {
                let flow = app.terminal_lost(&error.to_string());
                lost = Some(anyhow!(error).context("cannot draw to the terminal"));
                if flow != Flow::Continue {
                    return ended(lost);
                }
            }
        }
        let event = if app.needs_ticks() && lost.is_none() {
            match rx.recv_timeout(TICK) {
                Ok(event) => event,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return ended(lost),
            }
        } else {
            match rx.recv() {
                Ok(event) => event,
                Err(_) => return ended(lost),
            }
        };
        // Handle everything already queued before drawing again, so a burst
        // of progress messages costs one frame.
        for event in std::iter::once(event).chain(rx.try_iter()) {
            if app.handle(event) != Flow::Continue {
                return ended(lost);
            }
        }
    }
}

/// Waits until no task that changes files runs.
fn wait_for_writes(spawner: &Spawner) {
    while spawner.writes_in_progress() > 0 {
        std::thread::sleep(WRITE_POLL);
    }
}

/// Reads keys on their own thread; a read error is reported once, then the thread ends.
fn read_terminal_events(tx: mpsc::Sender<LoopEvent>) -> Result<()> {
    std::thread::Builder::new()
        .name("sift-input".into())
        .spawn(move || loop {
            let event = match crossterm::event::read() {
                Ok(event) => LoopEvent::Terminal(event),
                Err(error) => {
                    let _ = tx.send(LoopEvent::InputLost(error.to_string()));
                    break;
                }
            };
            if tx.send(event).is_err() {
                break;
            }
        })
        .context("cannot start reading keys")?;
    Ok(())
}

fn read_free_space() -> Option<u64> {
    let disks = tiny_core::sys::disks();
    tiny_core::sys::startup_capacity(&disks).map(|volume| volume.available_bytes)
}
