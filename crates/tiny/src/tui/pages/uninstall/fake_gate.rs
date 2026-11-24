//! A gate the fake engine holds a call at, so a test sees the state while it
//! waits; the test lets calls through one permit at a time.

use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// Holds calls until permits are given; says which app entered.
pub struct Gate {
    permits: Mutex<usize>,
    open: Condvar,
    entered: Mutex<Sender<String>>,
}

pub struct GateHandle {
    gate: Arc<Gate>,
    entered: Receiver<String>,
}

impl GateHandle {
    /// The name of the next app that reached the gate.
    pub fn wait_entered(&self) -> String {
        self.entered
            .recv_timeout(Duration::from_secs(10))
            .expect("a call reached the gate")
    }

    pub fn release(&self, count: usize) {
        *self.gate.permits.lock().unwrap() += count;
        self.gate.open.notify_all();
    }
}

impl Gate {
    pub fn new() -> (Arc<Gate>, GateHandle) {
        let (tx, rx) = mpsc::channel();
        let gate = Arc::new(Gate {
            permits: Mutex::new(0),
            open: Condvar::new(),
            entered: Mutex::new(tx),
        });
        let handle = GateHandle {
            gate: gate.clone(),
            entered: rx,
        };
        (gate, handle)
    }

    pub fn pass(&self, name: &str) {
        let _ = self.entered.lock().unwrap().send(name.to_string());
        let mut permits = self.permits.lock().unwrap();
        while *permits == 0 {
            permits = self.open.wait(permits).unwrap();
        }
        *permits -= 1;
    }
}
