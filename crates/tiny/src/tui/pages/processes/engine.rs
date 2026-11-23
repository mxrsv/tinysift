//! What Processes reads and the one call that acts, behind a trait so tests
//! never sample or signal real processes.

use std::collections::HashMap;

use tiny_core::processes::{
    self, Listener, ParentState, ProcessInfo, ProcessSnapshot, Refusal, Sampler, TerminateKind,
    TerminateOutcome, TerminateTarget,
};
use tiny_core::runner::RealRunner;

/// One process with what its detail shows.
#[derive(Clone, Debug, PartialEq)]
pub struct ProcessRow {
    pub info: ProcessInfo,
    pub parent: ParentState,
    pub children: usize,
    pub ports: Ports,
}

/// Listening TCP ports, as far as `lsof` run as this user can see them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Ports {
    Listening(Vec<u16>),
    /// Another user's process: a non-root `lsof` reports nothing for it.
    NotVisible,
    /// The lookup failed or timed out.
    Unavailable,
}

pub trait ProcessEngine: Send + Sync + 'static {
    /// Every process in one measured sample (about half a second); runs on a worker thread.
    fn read(&self) -> Vec<ProcessRow>;
    /// Why the engine will not signal this process, if it will not.
    fn refusal(&self, info: &ProcessInfo) -> Option<Refusal>;
    /// The engine's guarded call: revalidates PID and start time, then
    /// signals. Blocks up to two seconds; runs on a worker thread.
    fn terminate(
        &self,
        target: &TerminateTarget,
        kind: TerminateKind,
    ) -> Result<TerminateOutcome, String>;
}

/// The calls `sift processes` and `sift processes quit` make.
pub struct RealProcesses;

impl ProcessEngine for RealProcesses {
    fn read(&self) -> Vec<ProcessRow> {
        let mut sampler = Sampler::new();
        let mut sampled = None;
        // One `lsof` for every listener, then the sample, as `listeners` orders them.
        let listed = processes::listeners(&RealRunner, || {
            let snapshot = sampler.sample_measured();
            sampled = Some(snapshot.clone());
            snapshot
        });
        let snapshot = sampled.unwrap_or_else(|| sampler.sample_measured());
        let listeners = listed.ok().map(|found| found.listeners);
        rows_from(&snapshot, listeners.as_deref())
    }

    fn refusal(&self, info: &ProcessInfo) -> Option<Refusal> {
        processes::refusal(info)
    }

    fn terminate(
        &self,
        target: &TerminateTarget,
        kind: TerminateKind,
    ) -> Result<TerminateOutcome, String> {
        processes::terminate(target, kind).map_err(|error| error.to_string())
    }
}

/// Each sampled process with its parent, its number of children and its
/// listening ports; `listeners` is `None` when the lookup failed.
pub fn rows_from(snapshot: &ProcessSnapshot, listeners: Option<&[Listener]>) -> Vec<ProcessRow> {
    let names: HashMap<u32, &str> = snapshot
        .processes
        .iter()
        .map(|p| (p.pid, p.name.as_str()))
        .collect();
    let mut children: HashMap<u32, usize> = HashMap::new();
    for parent in snapshot.processes.iter().filter_map(|p| p.parent_pid) {
        *children.entry(parent).or_default() += 1;
    }
    let mut ports: HashMap<u32, Vec<u16>> = HashMap::new();
    for listener in listeners.unwrap_or_default() {
        let list = ports.entry(listener.pid).or_default();
        if !list.contains(&listener.port) {
            list.push(listener.port);
        }
    }
    snapshot
        .processes
        .iter()
        .map(|info| ProcessRow {
            parent: match info.parent_pid {
                None => ParentState::None,
                Some(pid) => match names.get(&pid) {
                    Some(name) => ParentState::Running {
                        pid,
                        name: (*name).to_string(),
                    },
                    None => ParentState::Exited { pid },
                },
            },
            children: children.get(&info.pid).copied().unwrap_or(0),
            ports: if !info.is_current_user {
                Ports::NotVisible
            } else if listeners.is_none() {
                Ports::Unavailable
            } else {
                let mut own = ports.get(&info.pid).cloned().unwrap_or_default();
                own.sort_unstable();
                Ports::Listening(own)
            },
            info: info.clone(),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::process::{Child, Command};

    fn info(pid: u32, name: &str, parent: Option<u32>, mine: bool) -> ProcessInfo {
        ProcessInfo {
            pid,
            name: name.into(),
            user: None,
            uid: None,
            is_current_user: mine,
            parent_pid: parent,
            start_time: 0,
            cpu_percent: None,
            cpu_measured: false,
            memory_bytes: None,
            footprint_bytes: None,
            cpu_time_ns: None,
            responsible: None,
            exe: None,
        }
    }

    fn listener(pid: u32, port: u16) -> Listener {
        Listener {
            port,
            address: "127.0.0.1".into(),
            pid,
            process: None,
            refusal: None,
            actionable: false,
        }
    }

    #[test]
    fn rows_carry_parent_children_and_ports_from_one_sample() {
        let snapshot = ProcessSnapshot {
            processes: vec![
                info(1240, "Google Chrome", Some(1), true),
                info(17703, "Renderer", Some(1240), true),
                info(17704, "Helper", Some(1240), true),
                info(88, "root thing", Some(9), false),
            ],
            sampled_at: 0,
            cpu_measured: true,
            system_usage: Default::default(),
        };
        let listeners = [
            listener(17703, 9229),
            listener(17703, 3000),
            listener(17703, 3000),
        ];
        let rows = rows_from(&snapshot, Some(&listeners));
        assert_eq!(rows[0].parent, ParentState::Exited { pid: 1 });
        assert_eq!(rows[0].children, 2);
        assert_eq!(
            rows[1].parent,
            ParentState::Running {
                pid: 1240,
                name: "Google Chrome".into()
            }
        );
        assert_eq!(rows[1].ports, Ports::Listening(vec![3000, 9229]));
        assert_eq!(rows[2].ports, Ports::Listening(vec![]));
        assert_eq!(rows[3].ports, Ports::NotVisible);
        let failed = rows_from(&snapshot, None);
        assert_eq!(failed[1].ports, Ports::Unavailable);
    }

    #[test]
    fn the_real_engine_refuses_what_the_command_refuses() {
        let engine = RealProcesses;
        let parent = std::os::unix::process::parent_id();
        assert_eq!(
            engine.refusal(&info(std::process::id(), "sift", None, true)),
            Some(Refusal::OwnProcess)
        );
        assert_eq!(
            engine.refusal(&info(parent, "zsh", None, true)),
            Some(Refusal::OwnParent)
        );
        assert_eq!(
            engine.refusal(&info(1, "launchd", None, true)),
            Some(Refusal::SystemProcess)
        );
        assert_eq!(
            engine.refusal(&info(4242, "x", None, false)),
            Some(Refusal::OtherUser)
        );
        for name in [
            "Finder",
            "Dock",
            "WindowServer",
            "loginwindow",
            "SystemUIServer",
        ] {
            assert!(
                matches!(
                    engine.refusal(&info(4242, name, None, true)),
                    Some(Refusal::Protected { .. })
                ),
                "{name}"
            );
        }
    }

    /// Kills and reaps the child even when an assertion fails first.
    struct Disposable(Child);

    impl Drop for Disposable {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[test]
    fn the_real_engine_checks_identity_before_it_signals() {
        // Only a child this test spawned and owns is ever addressed.
        let mut child = Disposable(Command::new("/bin/sleep").arg("30").spawn().unwrap());
        let target = TerminateTarget {
            pid: child.0.id(),
            start_time: 0,
            name: "sleep".into(),
            exe: None,
        };
        let outcome = RealProcesses.terminate(&target, TerminateKind::Force);
        assert_eq!(outcome, Ok(TerminateOutcome::IdentityChanged));
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "a stale identity sends nothing"
        );
    }
}
