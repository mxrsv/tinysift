//! Background work for pages and the one channel the event loop drains.
//!
//! A page never blocks the UI thread: it spawns a task with
//! [`PageCtx::spawn`](super::pages::PageCtx::spawn), the task sends typed
//! messages through its [`Worker`], and the loop hands each message back to
//! the page that spawned it, on the UI thread, between frames.

use std::any::Any;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use super::pages::PageId;

/// Everything that wakes the event loop.
pub enum LoopEvent {
    Terminal(crossterm::event::Event),
    /// A message a page's task sent; the page downcasts it to its own type.
    #[cfg_attr(
        not(test),
        allow(
            dead_code,
            reason = "sent once a page spawns work, plan tasks 05 to 09"
        )
    )]
    Page {
        page: PageId,
        message: Box<dyn Any + Send>,
    },
    /// A page's task panicked; the reason is shown to the page and after exit.
    TaskFailed {
        page: PageId,
        reason: String,
    },
    /// The free-disk figure for the tab bar.
    FreeSpace(Option<u64>),
    /// SIGTERM, SIGHUP or SIGINT: quit as `q` does.
    Signal,
}

/// The sending side shared by every task, and a count of tasks still running.
#[derive(Clone)]
pub struct Spawner {
    tx: Sender<LoopEvent>,
    live: Arc<AtomicUsize>,
}

impl Spawner {
    pub fn new(tx: Sender<LoopEvent>) -> Self {
        Self {
            tx,
            live: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// Tasks spawned and not yet finished.
    #[cfg_attr(not(test), expect(dead_code, reason = "read by the page test harness"))]
    pub fn live_tasks(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }

    /// Runs `job` on its own thread. A panic inside it is caught and reported
    /// as [`LoopEvent::TaskFailed`] instead of tearing down the terminal.
    pub fn spawn<M, F>(&self, page: PageId, job: F) -> std::io::Result<TaskHandle>
    where
        M: Send + 'static,
        F: FnOnce(Worker<M>) + Send + 'static,
    {
        let cancel = Arc::new(AtomicBool::new(false));
        let worker = Worker {
            tx: self.tx.clone(),
            page,
            cancel: cancel.clone(),
            _message: PhantomData,
        };
        let name = format!("sift-{}", page.title().to_lowercase());
        self.run_counted(name, move |tx| {
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(worker)));
            if let Err(payload) = outcome {
                let _ = tx.send(LoopEvent::TaskFailed {
                    page,
                    reason: panic_reason(payload.as_ref()),
                });
            }
        })?;
        Ok(TaskHandle { cancel })
    }

    /// Runs `job` on a named thread counted in [`live_tasks`](Self::live_tasks).
    pub fn run_counted<F>(&self, name: String, job: F) -> std::io::Result<()>
    where
        F: FnOnce(&Sender<LoopEvent>) + Send + 'static,
    {
        let tx = self.tx.clone();
        let live = LiveTask::start(self.live.clone());
        std::thread::Builder::new().name(name).spawn(move || {
            let _live = live;
            job(&tx);
        })?;
        Ok(())
    }
}

/// Decrements the live count when the task's thread ends, however it ends.
struct LiveTask(Arc<AtomicUsize>);

impl LiveTask {
    fn start(live: Arc<AtomicUsize>) -> Self {
        live.fetch_add(1, Ordering::SeqCst);
        Self(live)
    }
}

impl Drop for LiveTask {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

fn panic_reason(payload: &(dyn Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "a background task stopped unexpectedly".into())
}

/// The task's side: send results, and check whether the page asked it to stop.
#[cfg_attr(
    not(test),
    allow(dead_code, reason = "handed to page tasks, plan tasks 05 to 09")
)]
pub struct Worker<M> {
    tx: Sender<LoopEvent>,
    page: PageId,
    cancel: Arc<AtomicBool>,
    _message: PhantomData<fn(M)>,
}

impl<M: Send + 'static> Worker<M> {
    /// Hands `message` to the page. `false` means the TUI has closed: stop.
    pub fn send(&self, message: M) -> bool {
        self.tx
            .send(LoopEvent::Page {
                page: self.page,
                message: Box::new(message),
            })
            .is_ok()
    }

    /// True once the page called [`TaskHandle::cancel`] (esc, or quitting).
    /// Check it between steps; never abandon a step half done.
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    /// The same flag, for core calls that take one (`ScanContext::new(Some(flag), …)`).
    pub fn cancel_flag(&self) -> &AtomicBool {
        &self.cancel
    }
}

/// The page's side of a task: ask it to stop at its next check.
#[derive(Clone, Debug)]
pub struct TaskHandle {
    cancel: Arc<AtomicBool>,
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "kept by pages that can stop work, plan tasks 06 and 07"
    )
)]
impl TaskHandle {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn messages_reach_the_loop_tagged_with_their_page() {
        let (tx, rx) = mpsc::channel();
        let spawner = Spawner::new(tx);
        spawner
            .spawn(PageId::Scan, |worker: Worker<u32>| {
                assert!(worker.send(7));
            })
            .unwrap();
        match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
            LoopEvent::Page { page, message } => {
                assert_eq!(page, PageId::Scan);
                assert_eq!(*message.downcast::<u32>().unwrap(), 7);
            }
            _ => panic!("expected a page message"),
        }
    }

    #[test]
    fn cancel_reaches_the_task() {
        let (tx, rx) = mpsc::channel();
        let spawner = Spawner::new(tx);
        let (go, wait) = mpsc::channel::<()>();
        let handle = spawner
            .spawn(PageId::Clean, move |worker: Worker<bool>| {
                wait.recv().unwrap();
                worker.send(worker.is_cancelled() && worker.cancel_flag().load(Ordering::SeqCst));
            })
            .unwrap();
        handle.cancel();
        assert!(handle.is_cancelled());
        go.send(()).unwrap();
        match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
            LoopEvent::Page { message, .. } => assert!(*message.downcast::<bool>().unwrap()),
            _ => panic!("expected a page message"),
        }
    }

    #[test]
    fn a_panicking_task_is_reported_and_no_longer_counted() {
        let (tx, rx) = mpsc::channel();
        let spawner = Spawner::new(tx);
        spawner
            .spawn(PageId::System, |_worker: Worker<()>| {
                panic!("sampler broke")
            })
            .unwrap();
        match rx.recv_timeout(Duration::from_secs(5)).unwrap() {
            LoopEvent::TaskFailed { page, reason } => {
                assert_eq!(page, PageId::System);
                assert_eq!(reason, "sampler broke");
            }
            _ => panic!("expected a failure"),
        }
        for _ in 0..500 {
            if spawner.live_tasks() == 0 {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the task is still counted");
    }

    #[test]
    fn send_reports_a_closed_tui() {
        let (tx, rx) = mpsc::channel();
        let spawner = Spawner::new(tx);
        drop(rx);
        let (done_tx, done_rx) = mpsc::channel();
        spawner
            .spawn(PageId::Scan, move |worker: Worker<u8>| {
                done_tx.send(worker.send(1)).unwrap();
            })
            .unwrap();
        assert!(!done_rx.recv_timeout(Duration::from_secs(5)).unwrap());
    }
}
