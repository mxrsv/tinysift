//! Background work for pages and the one channel the event loop drains.
//!
//! A page never blocks the UI thread: it spawns a task with
//! [`PageCtx::spawn`](super::pages::PageCtx::spawn), the task sends typed
//! messages through its [`Worker`], and the loop hands each message back to
//! the page that spawned it, on the UI thread, between frames.

use std::any::Any;
use std::marker::PhantomData;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;

use super::pages::PageId;

/// Which signal asked the session to end.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SignalKind {
    /// SIGHUP: the terminal went away. It never cuts a run short.
    Hangup,
    /// SIGINT or SIGTERM: someone asked to quit; a second one quits at once.
    Terminate,
}

/// Everything that wakes the event loop.
pub enum LoopEvent {
    Terminal(crossterm::event::Event),
    /// Keys can no longer be read (the terminal is gone); the reason.
    InputLost(String),
    /// A message a page's task sent; the page downcasts it to its own type.
    Page {
        page: PageId,
        message: Box<dyn Any + Send>,
    },
    /// A page's task panicked; the reason is shown to the page and after exit.
    TaskFailed {
        page: PageId,
        task: TaskId,
        reason: String,
    },
    /// The free-disk figure for the tab bar.
    FreeSpace(Option<u64>),
    Signal(SignalKind),
}

/// Which task a handle or a failure belongs to, so a page can tell its
/// current task from one it stopped or replaced.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct TaskId(u64);

/// The sending side shared by every task, a count of tasks still running,
/// and a count of those that change files.
#[derive(Clone)]
pub struct Spawner {
    tx: Sender<LoopEvent>,
    live: Arc<AtomicUsize>,
    writes: Arc<AtomicUsize>,
    next_id: Arc<AtomicU64>,
}

impl Spawner {
    pub fn new(tx: Sender<LoopEvent>) -> Self {
        Self {
            tx,
            live: Arc::new(AtomicUsize::new(0)),
            writes: Arc::new(AtomicUsize::new(0)),
            next_id: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Tasks spawned and not yet finished.
    #[cfg(test)]
    pub fn live_tasks(&self) -> usize {
        self.live.load(Ordering::SeqCst)
    }

    /// Tasks spawned with [`spawn_write`](Self::spawn_write) and not yet finished.
    pub fn writes_in_progress(&self) -> usize {
        self.writes.load(Ordering::SeqCst)
    }

    /// Runs `job` on its own thread. A panic inside it is caught and reported
    /// as [`LoopEvent::TaskFailed`] instead of tearing down the terminal.
    pub fn spawn<M, F>(&self, page: PageId, job: F) -> std::io::Result<TaskHandle>
    where
        M: Send + 'static,
        F: FnOnce(Worker<M>) + Send + 'static,
    {
        self.spawn_counted(page, job, None)
    }

    /// [`spawn`](Self::spawn) for a task that changes files: the process does
    /// not end while one runs, even when the UI thread fails (I14).
    pub fn spawn_write<M, F>(&self, page: PageId, job: F) -> std::io::Result<TaskHandle>
    where
        M: Send + 'static,
        F: FnOnce(Worker<M>) + Send + 'static,
    {
        self.spawn_counted(page, job, Some(LiveTask::start(self.writes.clone())))
    }

    fn spawn_counted<M, F>(
        &self,
        page: PageId,
        job: F,
        write: Option<LiveTask>,
    ) -> std::io::Result<TaskHandle>
    where
        M: Send + 'static,
        F: FnOnce(Worker<M>) + Send + 'static,
    {
        let id = TaskId(self.next_id.fetch_add(1, Ordering::SeqCst));
        let cancel = Arc::new(AtomicBool::new(false));
        let worker = Worker {
            tx: self.tx.clone(),
            page,
            cancel: cancel.clone(),
            _message: PhantomData,
        };
        let name = format!("sift-{}", page.title().to_lowercase());
        self.run_counted(name, move |tx| {
            let _write = write;
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| job(worker)));
            if let Err(payload) = outcome {
                let _ = tx.send(LoopEvent::TaskFailed {
                    page,
                    task: id,
                    reason: panic_reason(payload.as_ref()),
                });
            }
        })?;
        Ok(TaskHandle { cancel, id })
    }

    /// Runs `job` on a named thread counted in `live_tasks`.
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
    id: TaskId,
}

impl TaskHandle {
    pub fn cancel(&self) {
        self.cancel.store(true, Ordering::SeqCst);
    }

    #[cfg(test)]
    pub fn is_cancelled(&self) -> bool {
        self.cancel.load(Ordering::SeqCst)
    }

    pub fn id(&self) -> TaskId {
        self.id
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
            LoopEvent::TaskFailed { page, reason, .. } => {
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
    fn a_failure_names_the_task_that_failed() {
        let (tx, rx) = mpsc::channel();
        let spawner = Spawner::new(tx);
        let first = spawner.spawn(PageId::Scan, |_: Worker<()>| {}).unwrap();
        let second = spawner
            .spawn(PageId::Scan, |_: Worker<()>| panic!("late walk"))
            .unwrap();
        assert_ne!(first.id(), second.id());
        let failed = rx
            .iter()
            .find_map(|event| match event {
                LoopEvent::TaskFailed { task, .. } => Some(task),
                _ => None,
            })
            .unwrap();
        assert_eq!(failed, second.id());
    }

    #[test]
    fn a_write_is_counted_until_its_thread_ends() {
        let (tx, _rx) = mpsc::channel();
        let spawner = Spawner::new(tx);
        let (go, wait) = mpsc::channel::<()>();
        spawner
            .spawn_write(PageId::Clean, move |_: Worker<()>| {
                wait.recv().unwrap();
                panic!("the step failed");
            })
            .unwrap();
        spawner.spawn(PageId::Scan, |_: Worker<()>| {}).unwrap();
        assert_eq!(spawner.writes_in_progress(), 1);
        go.send(()).unwrap();
        for _ in 0..500 {
            if spawner.writes_in_progress() == 0 {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the write is still counted");
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
