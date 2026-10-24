//! Process inspection and actions shared by the CLI and the native app.

pub mod ports;
pub mod pressure;
pub mod snapshot;
pub mod terminate;

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::runner::CommandRunner;

pub use ports::{
    listeners, port_owners, Listener, Listeners, ListeningPort, PortOwner, PortOwners,
    PORT_VISIBILITY_CAVEAT,
};
pub use pressure::MemoryPressure;
pub use snapshot::{LoadAverage, ProcessInfo, ProcessSnapshot, Sampler, SystemUsage};
pub use terminate::{
    refusal, terminate, Refusal, TerminateKind, TerminateOutcome, TerminateTarget,
};

/// Diagnostic used when ports are not probed for another user's process: a
/// non-root `lsof` would silently report nothing for it.
pub const PORTS_OTHER_USER: &str = "not permitted: process belongs to another user";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProcessSort {
    Cpu,
    Memory,
}

/// Sorts descending; unavailable measurements sort last.
pub fn sort_processes(processes: &mut [ProcessInfo], sort: ProcessSort) {
    match sort {
        ProcessSort::Cpu => processes.sort_by(|a, b| {
            let (a, b) = (a.cpu_percent.unwrap_or(-1.0), b.cpu_percent.unwrap_or(-1.0));
            b.total_cmp(&a)
        }),
        ProcessSort::Memory => processes.sort_by_key(|p| std::cmp::Reverse(p.memory_bytes)),
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum ParentState {
    /// The process has no parent (for example launchd).
    None,
    Running {
        pid: u32,
        name: String,
    },
    /// The recorded parent is no longer running.
    Exited {
        pid: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessDetail {
    #[serde(flatten)]
    pub process: ProcessInfo,
    pub parent: ParentState,
    pub children: Vec<ProcessInfo>,
    /// `None` when the probe failed or was not permitted; see `ports_error`.
    pub ports: Option<Vec<ListeningPort>>,
    pub ports_error: Option<String>,
}

pub fn detail(
    snapshot: &ProcessSnapshot,
    pid: u32,
    runner: &dyn CommandRunner,
) -> Result<ProcessDetail> {
    let process = snapshot
        .processes
        .iter()
        .find(|p| p.pid == pid)
        .cloned()
        .ok_or_else(|| {
            Error::InvalidInput(format!("process {pid} is not in the current sample"))
        })?;
    let parent = match process.parent_pid {
        None => ParentState::None,
        Some(parent_pid) => match snapshot.processes.iter().find(|p| p.pid == parent_pid) {
            Some(parent) => ParentState::Running {
                pid: parent_pid,
                name: parent.name.clone(),
            },
            None => ParentState::Exited { pid: parent_pid },
        },
    };
    let children = snapshot
        .processes
        .iter()
        .filter(|p| p.parent_pid == Some(pid))
        .cloned()
        .collect();
    let (ports, ports_error) = if !process.is_current_user {
        (None, Some(PORTS_OTHER_USER.to_string()))
    } else {
        match ports::listening_ports(pid, runner) {
            Ok(ports) => (Some(ports), None),
            Err(error) => (None, Some(error)),
        }
    };
    Ok(ProcessDetail {
        process,
        parent,
        children,
        ports,
        ports_error,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::test_support::MockRunner;
    use crate::runner::{CommandError, CommandOutcome};

    fn info(pid: u32, parent: Option<u32>, mine: bool) -> ProcessInfo {
        ProcessInfo {
            pid,
            name: format!("p{pid}"),
            user: None,
            uid: Some(if mine { 501 } else { 0 }),
            is_current_user: mine,
            parent_pid: parent,
            start_time: 0,
            cpu_percent: Some(pid as f32),
            cpu_measured: true,
            memory_bytes: Some(u64::from(pid)),
            footprint_bytes: None,
            exe: None,
        }
    }

    fn snapshot(processes: Vec<ProcessInfo>) -> ProcessSnapshot {
        ProcessSnapshot {
            processes,
            sampled_at: 0,
            cpu_measured: true,
            system_usage: Default::default(),
        }
    }

    fn lsof_args(pid: &str) -> [&str; 7] {
        ["-nP", "-a", "-p", pid, "-iTCP", "-sTCP:LISTEN", "-Fn"]
    }

    #[test]
    fn detail_reports_exited_parent_and_children() {
        let snap = snapshot(vec![info(10, Some(9), true), info(11, Some(10), true)]);
        let runner = MockRunner::new().with_output(
            ports::LSOF,
            &lsof_args("10"),
            Ok(CommandOutcome {
                status: Some(1),
                stdout: String::new(),
                stderr: String::new(),
            }),
        );
        let detail = detail(&snap, 10, &runner).unwrap();
        assert_eq!(detail.parent, ParentState::Exited { pid: 9 });
        assert_eq!(
            detail.children.iter().map(|c| c.pid).collect::<Vec<_>>(),
            vec![11]
        );
        assert_eq!(detail.ports, Some(Vec::new()));
        assert_eq!(detail.ports_error, None);
    }

    #[test]
    fn other_users_process_skips_the_port_probe() {
        let snap = snapshot(vec![info(1, None, false), info(20, Some(1), false)]);
        let runner = MockRunner::new();
        let detail = detail(&snap, 20, &runner).unwrap();
        assert_eq!(
            detail.parent,
            ParentState::Running {
                pid: 1,
                name: "p1".into()
            }
        );
        assert_eq!(detail.ports, None);
        assert_eq!(detail.ports_error.as_deref(), Some(PORTS_OTHER_USER));
        assert!(runner.output_calls.lock().unwrap().is_empty());
    }

    #[test]
    fn failed_port_probe_is_explicit() {
        let snap = snapshot(vec![info(30, None, true)]);
        let runner = MockRunner::new().with_output(
            ports::LSOF,
            &lsof_args("30"),
            Err(CommandError::Timeout {
                bin: ports::LSOF.into(),
                timeout_ms: 3000,
            }),
        );
        let detail = detail(&snap, 30, &runner).unwrap();
        assert_eq!(detail.parent, ParentState::None);
        assert_eq!(detail.ports, None);
        assert!(detail.ports_error.is_some());
    }

    #[test]
    fn unknown_pid_is_invalid_input() {
        let result = detail(&snapshot(vec![]), 99, &MockRunner::new());
        assert!(matches!(result, Err(Error::InvalidInput(_))));
    }

    #[test]
    fn sorting_puts_unavailable_last() {
        let mut unreadable = info(5, None, false);
        unreadable.cpu_percent = None;
        unreadable.memory_bytes = None;
        let mut processes = vec![unreadable, info(2, None, true), info(7, None, true)];
        sort_processes(&mut processes, ProcessSort::Cpu);
        assert_eq!(
            processes.iter().map(|p| p.pid).collect::<Vec<_>>(),
            vec![7, 2, 5]
        );
        sort_processes(&mut processes, ProcessSort::Memory);
        assert_eq!(
            processes.iter().map(|p| p.pid).collect::<Vec<_>>(),
            vec![7, 2, 5]
        );
    }

    /// Real sysinfo samples against a disposable busy child process.
    #[test]
    fn sampler_measures_a_disposable_busy_child() {
        let mut child = std::process::Command::new("/usr/bin/yes")
            .stdout(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let child_pid = child.id();
        let mut sampler = Sampler::new();
        let first = sampler.sample();
        let measured = sampler.sample_measured();
        let found = measured
            .processes
            .iter()
            .find(|p| p.pid == child_pid)
            .cloned();
        child.kill().unwrap();
        child.wait().unwrap();

        let unmeasured = first.processes.iter().find(|p| p.pid == child_pid).unwrap();
        assert_eq!(unmeasured.cpu_percent, None);
        let found = found.expect("child appears in the sample");
        assert!(measured.cpu_measured);
        assert_eq!(found.parent_pid, Some(std::process::id()));
        assert!(found.is_current_user);
        assert!(found.memory_bytes.is_some());
        assert!(found.cpu_measured);
        assert!(
            found.cpu_percent.unwrap() > 1.0,
            "busy child reads {:?}",
            found.cpu_percent
        );

        let after = sampler.sample();
        assert!(after.processes.iter().all(|p| p.pid != child_pid));
    }
}
