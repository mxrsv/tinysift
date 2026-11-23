//! Entering and leaving the full-screen terminal, and putting it back on
//! every way out: normal exit (the guard), a panic on the UI thread (the
//! hook), and SIGTERM/SIGHUP/SIGINT (the loop quits as `q` does, then the
//! guard runs). Ctrl-C arrives as a key in raw mode and takes the `q` path.

use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;

use crossterm::cursor::{Hide, Show};
use crossterm::execute;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};

use super::work::LoopEvent;

/// True between entering and restoring the real terminal.
static ACTIVE: AtomicBool = AtomicBool::new(false);

/// Switching the terminal in and out of the TUI's mode.
pub trait TtyControl {
    fn enter(&mut self) -> io::Result<()>;
    fn leave(&mut self) -> io::Result<()>;
}

/// The process's own terminal: raw mode, alternate screen, hidden cursor.
pub struct RealTty;

impl TtyControl for RealTty {
    fn enter(&mut self) -> io::Result<()> {
        ACTIVE.store(true, Ordering::SeqCst);
        enable_raw_mode()?;
        write_enter(&mut io::stdout())
    }

    fn leave(&mut self) -> io::Result<()> {
        restore_real_terminal()
    }
}

/// Restores the real terminal once; later calls do nothing.
fn restore_real_terminal() -> io::Result<()> {
    if !ACTIVE.swap(false, Ordering::SeqCst) {
        return Ok(());
    }
    let raw = disable_raw_mode();
    let screen = write_leave(&mut io::stdout());
    raw.and(screen)
}

fn write_enter(out: &mut impl Write) -> io::Result<()> {
    execute!(out, EnterAlternateScreen, Hide)
}

/// Shows the cursor and leaves the alternate screen.
fn write_leave(out: &mut impl Write) -> io::Result<()> {
    execute!(out, Show, LeaveAlternateScreen)
}

/// Holds the terminal in TUI mode; dropping it (also while unwinding) restores it.
pub struct ScreenGuard<T: TtyControl> {
    tty: T,
    entered: bool,
}

impl<T: TtyControl> ScreenGuard<T> {
    pub fn enter(mut tty: T) -> io::Result<Self> {
        if let Err(error) = tty.enter() {
            let _ = tty.leave();
            return Err(error);
        }
        Ok(Self { tty, entered: true })
    }

    /// Restores now, reporting a failure that `Drop` would have to ignore.
    pub fn leave(mut self) -> io::Result<()> {
        self.entered = false;
        self.tty.leave()
    }
}

impl<T: TtyControl> Drop for ScreenGuard<T> {
    fn drop(&mut self) {
        if self.entered {
            let _ = self.tty.leave();
        }
    }
}

/// A panic on the UI thread restores the terminal before its message is
/// printed, so the message lands on the normal screen. A panic on a page's
/// worker thread leaves the screen alone: the worker reports it to the page
/// instead (`LoopEvent::TaskFailed`). Outside a session the hook only passes on.
pub fn install_panic_hook() {
    let ui_thread = std::thread::current().id();
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let session = ACTIVE.load(Ordering::SeqCst);
        if session && std::thread::current().id() != ui_thread {
            return;
        }
        let _ = restore_real_terminal();
        previous(info);
    }));
}

/// SIGTERM, SIGHUP and SIGINT become [`LoopEvent::Signal`], handled as `q`.
pub fn forward_signals(tx: Sender<LoopEvent>) -> io::Result<()> {
    use signal_hook::consts::{SIGHUP, SIGINT, SIGTERM};
    let mut signals = signal_hook::iterator::Signals::new([SIGTERM, SIGHUP, SIGINT])?;
    std::thread::Builder::new()
        .name("sift-signals".into())
        .spawn(move || {
            for _ in signals.forever() {
                if tx.send(LoopEvent::Signal).is_err() {
                    break;
                }
            }
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    #[derive(Clone, Default)]
    struct FakeTty {
        calls: Arc<Mutex<Vec<&'static str>>>,
        fail_enter: bool,
    }

    impl TtyControl for FakeTty {
        fn enter(&mut self) -> io::Result<()> {
            self.calls.lock().unwrap().push("enter");
            if self.fail_enter {
                Err(io::Error::other("no tty"))
            } else {
                Ok(())
            }
        }

        fn leave(&mut self) -> io::Result<()> {
            self.calls.lock().unwrap().push("leave");
            Ok(())
        }
    }

    #[test]
    fn leaving_restores_once() {
        let tty = FakeTty::default();
        ScreenGuard::enter(tty.clone()).unwrap().leave().unwrap();
        assert_eq!(*tty.calls.lock().unwrap(), ["enter", "leave"]);
    }

    #[test]
    fn a_panic_inside_the_session_still_restores_the_terminal() {
        let tty = FakeTty::default();
        let inner = tty.clone();
        let outcome = std::panic::catch_unwind(move || {
            let _guard = ScreenGuard::enter(inner).unwrap();
            panic!("drawing failed");
        });
        assert!(outcome.is_err());
        assert_eq!(*tty.calls.lock().unwrap(), ["enter", "leave"]);
    }

    #[test]
    fn a_failed_enter_undoes_what_it_did() {
        let tty = FakeTty {
            fail_enter: true,
            ..FakeTty::default()
        };
        assert!(ScreenGuard::enter(tty.clone()).is_err());
        assert_eq!(*tty.calls.lock().unwrap(), ["enter", "leave"]);
    }

    #[test]
    fn the_restore_sequence_shows_the_cursor_and_leaves_the_alternate_screen() {
        let mut out = Vec::new();
        write_leave(&mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("\x1b[?25h"), "{text:?}");
        assert!(text.contains("\x1b[?1049l"), "{text:?}");
        let mut out = Vec::new();
        write_enter(&mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        assert!(
            text.contains("\x1b[?1049h") && text.contains("\x1b[?25l"),
            "{text:?}"
        );
    }
}
