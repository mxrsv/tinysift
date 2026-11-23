//! The session: enter the terminal, draw, wait for one event, handle it,
//! repeat; leave the terminal before anything is printed.
//!
//! The loop sleeps until something happens: a key, a resize, a page's
//! message, a signal. Only while a page is busy does it also wake on a timer,
//! so the spinner turns; at rest nothing moves (FE8).

use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
use std::time::{Duration, Instant};

use anyhow::{Context, Result};
use ratatui::backend::{Backend, CrosstermBackend};
use ratatui::Terminal;

use super::app::{App, Flow, SessionEnd};
use super::icons::IconMode;
use super::pages::all_pages;
use super::terminal::{forward_signals, install_panic_hook, RealTty, ScreenGuard};
use super::theme::Theme;
use super::work::{LoopEvent, Spawner};

/// One spinner frame.
const TICK: Duration = Duration::from_millis(100);

/// Runs a whole session in the real terminal and returns what to print after it.
pub fn run(icons: IconMode, theme: Theme) -> Result<SessionEnd> {
    let (tx, rx) = mpsc::channel();
    forward_signals(tx.clone()).context("cannot listen for signals")?;
    install_panic_hook();
    let guard = ScreenGuard::enter(RealTty).context("cannot enter the full-screen view")?;
    let mut terminal = Terminal::new(CrosstermBackend::new(std::io::stdout()))?;
    read_terminal_events(tx.clone())?;
    let mut app = App::new(all_pages(), icons, Spawner::new(tx), read_free_space);
    app.start();
    let outcome = drive(&mut terminal, &mut app, &rx, &theme);
    drop(terminal);
    guard.leave().context("cannot restore the terminal")?;
    outcome?;
    Ok(app.finish())
}

/// Draws and handles events until the app says to exit or every sender is gone.
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
    loop {
        app.set_tick((started.elapsed().as_millis() / TICK.as_millis()) as u64);
        terminal.draw(|frame| {
            let area = frame.area();
            app.draw(frame.buffer_mut(), area, theme);
        })?;
        let event = if app.needs_ticks() {
            match rx.recv_timeout(TICK) {
                Ok(event) => event,
                Err(RecvTimeoutError::Timeout) => continue,
                Err(RecvTimeoutError::Disconnected) => return Ok(()),
            }
        } else {
            match rx.recv() {
                Ok(event) => event,
                Err(_) => return Ok(()),
            }
        };
        // Handle everything already queued before drawing again, so a burst
        // of progress messages costs one frame.
        for event in std::iter::once(event).chain(rx.try_iter()) {
            if app.handle(event) == Flow::Exit {
                return Ok(());
            }
        }
    }
}

fn read_terminal_events(tx: mpsc::Sender<LoopEvent>) -> Result<()> {
    std::thread::Builder::new()
        .name("sift-input".into())
        .spawn(move || {
            while let Ok(event) = crossterm::event::read() {
                if tx.send(LoopEvent::Terminal(event)).is_err() {
                    break;
                }
            }
        })
        .context("cannot start reading keys")?;
    Ok(())
}

fn read_free_space() -> Option<u64> {
    let disks = tiny_core::sys::disks();
    tiny_core::sys::startup_capacity(&disks).map(|volume| volume.available_bytes)
}
