//! A fake engine for the Processes tests, fed `PROCS` from `tools/mock.js`.
//! It records every quit and never samples or signals a real process.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use tiny_core::processes::{
    ParentState, ProcessInfo, Refusal, TerminateKind, TerminateOutcome, TerminateTarget,
};

use super::engine::{Ports, ProcessEngine, ProcessRow};

pub type Calls = Arc<Mutex<Vec<(TerminateTarget, TerminateKind)>>>;

pub struct FakeProcesses {
    pub rows: Vec<ProcessRow>,
    pub refused: Option<(u32, Refusal)>,
    pub outcome: TerminateOutcome,
    pub calls: Calls,
    pub reads: Arc<AtomicUsize>,
}

impl ProcessEngine for FakeProcesses {
    fn read(&self) -> Vec<ProcessRow> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        self.rows.clone()
    }

    fn refusal(&self, info: &ProcessInfo) -> Option<Refusal> {
        self.refused
            .as_ref()
            .filter(|(pid, _)| *pid == info.pid)
            .map(|(_, reason)| reason.clone())
    }

    fn terminate(
        &self,
        target: &TerminateTarget,
        kind: TerminateKind,
    ) -> Result<TerminateOutcome, String> {
        self.calls.lock().unwrap().push((target.clone(), kind));
        Ok(self.outcome.clone())
    }
}

/// `[pid, cpu, memory, name, parent, children, ports]`, as `PROCS` in `tools/mock.js`.
type MockRow = (
    u32,
    f32,
    Option<u64>,
    &'static str,
    (u32, &'static str),
    usize,
    &'static [u16],
);

fn rows(data: &[MockRow]) -> Vec<ProcessRow> {
    data.iter()
        .map(
            |&(pid, cpu, memory, name, (parent, parent_name), children, ports)| ProcessRow {
                info: ProcessInfo {
                    pid,
                    name: name.into(),
                    user: Some("someone".into()),
                    uid: Some(501),
                    is_current_user: true,
                    parent_pid: Some(parent),
                    start_time: 1_700_000_000 + u64::from(pid),
                    cpu_percent: Some(cpu),
                    cpu_measured: true,
                    memory_bytes: memory,
                    footprint_bytes: None,
                    cpu_time_ns: None,
                    responsible: None,
                    exe: None,
                },
                parent: ParentState::Running {
                    pid: parent,
                    name: parent_name.into(),
                },
                children,
                ports: Ports::Listening(ports.to_vec()),
            },
        )
        .collect()
}

const MB: u64 = 1_000_000;

pub fn normal() -> Vec<ProcessRow> {
    rows(&[
        (
            17703,
            39.0,
            Some(559 * MB),
            "Google Chrome Helper (Renderer)",
            (1240, "Google Chrome"),
            0,
            &[3000, 9229],
        ),
        (43154, 19.6, Some(89 * MB), "AltTab", (1, "launchd"), 0, &[]),
        (
            934,
            17.4,
            Some(111 * MB),
            "cfprefsd",
            (1, "launchd"),
            0,
            &[],
        ),
        (
            1548,
            13.9,
            Some(191 * MB),
            "Google Chrome Helper",
            (1240, "Google Chrome"),
            0,
            &[],
        ),
        (
            1240,
            8.1,
            Some(519 * MB),
            "Google Chrome",
            (1, "launchd"),
            14,
            &[],
        ),
        (
            92184,
            7.1,
            Some(601 * MB),
            "node (claude)",
            (88120, "zsh"),
            3,
            &[],
        ),
    ])
}

pub fn worst() -> Vec<ProcessRow> {
    rows(&[
        (
            99999,
            1187.4,
            Some(112_400 * MB),
            "com.apple.Virtualization.VirtualMachine (macOS 27 beta 4 clean install)",
            (4411, "UTM"),
            12,
            &[5900, 2222, 8080, 8443, 9000, 9001, 9002],
        ),
        (4, 100.0, Some(64_000 * MB), "x", (1, "launchd"), 0, &[]),
        (
            88412,
            99.9,
            Some(8_200 * MB),
            "rust-analyzer-proc-macro-srv",
            (88400, "rust-analyzer"),
            0,
            &[],
        ),
        (1, 0.0, None, "launchd", (0, "kernel_task"), 611, &[]),
        (
            612,
            0.0,
            Some(0),
            "node (codex) /Users/someone/Development/a-very-long-repository-name",
            (600, "zsh"),
            0,
            &[],
        ),
    ])
}
