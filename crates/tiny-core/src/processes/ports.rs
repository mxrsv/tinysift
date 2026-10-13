//! Listening TCP ports via bounded `lsof` probes: the ports of one process,
//! the visible owners of one port, and every visible listener.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::snapshot::{unix_now, ProcessInfo, ProcessSnapshot};
use super::terminate::{refusal, Refusal};
use crate::error::{Error, Result};
use crate::runner::CommandRunner;

pub const LSOF: &str = "/usr/sbin/lsof";
pub const LSOF_TIMEOUT: Duration = Duration::from_secs(3);

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ListeningPort {
    pub address: String,
    pub port: u16,
}

/// Returns the process's listening TCP ports, or a diagnostic when the probe
/// fails. `lsof` exits 1 with no output when nothing matches, which means
/// "no listening ports", not a failure.
pub fn listening_ports(
    pid: u32,
    runner: &dyn CommandRunner,
) -> std::result::Result<Vec<ListeningPort>, String> {
    let pid = pid.to_string();
    let args = ["-nP", "-a", "-p", &pid, "-iTCP", "-sTCP:LISTEN", "-Fn"];
    let stdout = run_lsof(&args, runner)?.unwrap_or_default();
    let mut ports: Vec<_> = parse_lsof(&stdout)
        .into_iter()
        .map(|(_, port)| port)
        .collect();
    ports.sort();
    ports.dedup();
    Ok(ports)
}

/// Runs `lsof`; `Ok(None)` when it exits 1 with no output, which means
/// "nothing matched", not a failure.
fn run_lsof(
    args: &[&str],
    runner: &dyn CommandRunner,
) -> std::result::Result<Option<String>, String> {
    let outcome = runner
        .output(LSOF, args, LSOF_TIMEOUT)
        .map_err(|error| error.to_string())?;
    let nothing_matched = outcome.status == Some(1)
        && outcome.stdout.trim().is_empty()
        && outcome.stderr.trim().is_empty();
    if nothing_matched {
        return Ok(None);
    }
    if !outcome.success() {
        return Err(format!(
            "lsof exited with status {:?}: {}",
            outcome.status,
            outcome.stderr.trim()
        ));
    }
    Ok(Some(outcome.stdout))
}

/// Always attached to port lookups: an empty owner list is not proof of a free port.
pub const PORT_VISIBILITY_CAVEAT: &str = "Only listeners visible to the current user are shown. \
Without root, lsof cannot see other users' sockets, so no owner here does not mean the port is free.";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortOwner {
    pub pid: u32,
    /// `None` when the PID was not in the sample taken after the probe.
    pub process: Option<ProcessInfo>,
    /// Why the owner cannot be quit, when it is in the sample.
    pub refusal: Option<Refusal>,
}

impl PortOwner {
    pub fn actionable(&self) -> bool {
        self.process.is_some() && self.refusal.is_none()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PortOwners {
    pub port: u16,
    /// Every visible owner; nothing is filtered out or signalled.
    pub owners: Vec<PortOwner>,
    pub visibility_caveat: String,
    pub sampled_at: u64,
    pub cpu_measured: bool,
}

/// Visible processes listening on TCP `port`. `lsof` runs first and `sample`
/// afterwards, so a listener that just started is in the sample. A failed or
/// timed-out probe is an error, never an empty result.
pub fn port_owners(
    port: u16,
    runner: &dyn CommandRunner,
    sample: impl FnOnce() -> ProcessSnapshot,
) -> Result<PortOwners> {
    if port == 0 {
        return Err(Error::InvalidInput(
            "port must be between 1 and 65535".into(),
        ));
    }
    let filter = format!("-iTCP:{port}");
    let lookup_started = unix_now();
    let stdout = run_lsof(&["-nP", &filter, "-sTCP:LISTEN", "-Fpn"], runner)
        .map_err(|detail| Error::Operation(format!("port {port} lookup failed: {detail}")))?;
    let mut pids: Vec<u32> = parse_lsof(&stdout.unwrap_or_default())
        .into_iter()
        .map(|(pid, _)| pid)
        .collect();
    pids.dedup();
    let snapshot = sample();
    let owners = pids
        .into_iter()
        .map(|pid| port_owner(pid, &snapshot, lookup_started))
        .collect();
    Ok(PortOwners {
        port,
        owners,
        visibility_caveat: PORT_VISIBILITY_CAVEAT.to_string(),
        sampled_at: snapshot.sampled_at,
        cpu_measured: snapshot.cpu_measured,
    })
}

/// `lookup_started` is the Unix time just before `lsof` ran. A sampled process
/// that started later may hold a reused PID, so it is never actionable.
fn port_owner(pid: u32, snapshot: &ProcessSnapshot, lookup_started: u64) -> PortOwner {
    let process = snapshot.processes.iter().find(|p| p.pid == pid).cloned();
    let refusal = process.as_ref().and_then(|process| {
        if process.start_time > lookup_started {
            Some(Refusal::IdentityUncertain)
        } else {
            refusal(process)
        }
    });
    PortOwner {
        pid,
        process,
        refusal,
    }
}

/// One visible listening socket and its owner, flattened for the Ports view.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Listener {
    pub port: u16,
    pub address: String,
    pub pid: u32,
    /// `None` when the PID was not in the sample taken after the probe.
    pub process: Option<ProcessInfo>,
    pub refusal: Option<Refusal>,
    /// In the sample and not refused, so Quit/Force Quit may be offered.
    pub actionable: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Listeners {
    /// Sorted by port, then PID, then address; one entry per socket address.
    pub listeners: Vec<Listener>,
    /// `PORT_VISIBILITY_CAVEAT`: the list is never complete.
    pub visibility_caveat: String,
    pub sampled_at: u64,
}

/// Every visible listening TCP socket. Same rules as `port_owners`: `lsof`
/// runs before `sample`, and a failed or timed-out probe is an error.
pub fn listeners(
    runner: &dyn CommandRunner,
    sample: impl FnOnce() -> ProcessSnapshot,
) -> Result<Listeners> {
    let lookup_started = unix_now();
    let stdout = run_lsof(&["-nP", "-iTCP", "-sTCP:LISTEN", "-Fpn"], runner)
        .map_err(|detail| Error::Operation(format!("listener lookup failed: {detail}")))?;
    let mut entries = parse_lsof(&stdout.unwrap_or_default());
    entries.sort_by(|(a_pid, a), (b_pid, b)| {
        (a.port, a_pid, &a.address).cmp(&(b.port, b_pid, &b.address))
    });
    let snapshot = sample();
    let listeners = entries
        .into_iter()
        .map(|(pid, socket)| {
            let owner = port_owner(pid, &snapshot, lookup_started);
            Listener {
                port: socket.port,
                address: socket.address,
                actionable: owner.actionable(),
                pid,
                process: owner.process,
                refusal: owner.refusal,
            }
        })
        .collect();
    Ok(Listeners {
        listeners,
        visibility_caveat: PORT_VISIBILITY_CAVEAT.to_string(),
        sampled_at: snapshot.sampled_at,
    })
}

/// Parses `lsof -F` output, where each `n<address:port>` line belongs to the
/// preceding `p<pid>` line. Splits on the last `:` so `[::1]:3000` and
/// `*:8080` both parse. Sorted by PID and deduplicated.
fn parse_lsof(stdout: &str) -> Vec<(u32, ListeningPort)> {
    let mut pid = None;
    let mut entries = Vec::new();
    for line in stdout.lines() {
        if let Some(value) = line.strip_prefix('p') {
            pid = value.parse().ok();
        } else if let (Some(pid), Some(name)) = (pid, line.strip_prefix('n')) {
            if let Some((address, port)) = name.rsplit_once(':') {
                if let Ok(port) = port.parse() {
                    let address = address.to_string();
                    entries.push((pid, ListeningPort { address, port }));
                }
            }
        }
    }
    entries.sort();
    entries.dedup();
    entries
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::test_support::MockRunner;
    use crate::runner::{CommandError, CommandOutcome};

    const ARGS: [&str; 7] = ["-nP", "-a", "-p", "42", "-iTCP", "-sTCP:LISTEN", "-Fn"];

    fn outcome(status: i32, stdout: &str, stderr: &str) -> CommandOutcome {
        CommandOutcome {
            status: Some(status),
            stdout: stdout.into(),
            stderr: stderr.into(),
        }
    }

    #[test]
    fn parses_ipv4_ipv6_and_wildcard_listeners() {
        let stdout = "p42\nf5\nn*:8080\nf6\nn127.0.0.1:5173\nf7\nn[::1]:3000\nf8\nn*:8080\n";
        let runner = MockRunner::new().with_output(LSOF, &ARGS, Ok(outcome(0, stdout, "")));
        assert_eq!(
            listening_ports(42, &runner).unwrap(),
            vec![
                ListeningPort {
                    address: "*".into(),
                    port: 8080
                },
                ListeningPort {
                    address: "127.0.0.1".into(),
                    port: 5173
                },
                ListeningPort {
                    address: "[::1]".into(),
                    port: 3000
                },
            ]
        );
    }

    #[test]
    fn exit_one_without_output_means_no_ports() {
        let runner = MockRunner::new().with_output(LSOF, &ARGS, Ok(outcome(1, "", "")));
        assert_eq!(listening_ports(42, &runner).unwrap(), Vec::new());
    }

    #[test]
    fn exit_one_with_stderr_is_an_error() {
        let runner = MockRunner::new().with_output(
            LSOF,
            &ARGS,
            Ok(outcome(1, "", "lsof: permission denied")),
        );
        assert!(listening_ports(42, &runner)
            .unwrap_err()
            .contains("permission denied"));
    }

    #[test]
    fn timeout_is_an_error_not_an_empty_list() {
        let timeout = CommandError::Timeout {
            bin: LSOF.into(),
            timeout_ms: 3000,
        };
        let runner = MockRunner::new().with_output(LSOF, &ARGS, Err(timeout));
        assert!(listening_ports(42, &runner)
            .unwrap_err()
            .contains("did not finish"));
    }

    const OWNER_ARGS: [&str; 4] = ["-nP", "-iTCP:8080", "-sTCP:LISTEN", "-Fpn"];
    const LISTENER_ARGS: [&str; 4] = ["-nP", "-iTCP", "-sTCP:LISTEN", "-Fpn"];

    fn sampled(pid: u32, mine: bool) -> ProcessInfo {
        ProcessInfo {
            pid,
            name: format!("p{pid}"),
            user: None,
            uid: None,
            is_current_user: mine,
            parent_pid: None,
            start_time: 0,
            cpu_percent: None,
            cpu_measured: false,
            memory_bytes: None,
            exe: None,
        }
    }

    fn sample() -> ProcessSnapshot {
        ProcessSnapshot {
            processes: vec![sampled(4100, true), sampled(4200, false)],
            sampled_at: 7,
            cpu_measured: true,
            system_usage: Default::default(),
        }
    }

    fn owners_with(
        result: std::result::Result<CommandOutcome, CommandError>,
    ) -> Result<PortOwners> {
        let runner = MockRunner::new().with_output(LSOF, &OWNER_ARGS, result);
        port_owners(8080, &runner, sample)
    }

    #[test]
    fn lists_every_owner_including_other_users_and_unsampled() {
        let stdout =
            "p4100\nf5\nn*:8080\nf6\nn[::1]:8080\np4200\nf6\nn*:8080\np4300\nf3\nn127.0.0.1:8080\n";
        let found = owners_with(Ok(outcome(0, stdout, ""))).unwrap();
        let summary: Vec<_> = found
            .owners
            .iter()
            .map(|o| {
                (
                    o.pid,
                    o.process.is_some(),
                    o.refusal.clone(),
                    o.actionable(),
                )
            })
            .collect();
        assert_eq!(
            summary,
            vec![
                (4100, true, None, true),
                (4200, true, Some(Refusal::OtherUser), false),
                (4300, false, None, false),
            ]
        );
        assert_eq!(found.visibility_caveat, PORT_VISIBILITY_CAVEAT);
        assert_eq!((found.port, found.sampled_at), (8080, 7));
    }

    #[test]
    fn owner_started_after_the_lookup_is_identity_uncertain() {
        let runner = MockRunner::new().with_output(
            LSOF,
            &OWNER_ARGS,
            Ok(outcome(0, "p4100\nf5\nn*:8080\n", "")),
        );
        let reused = || {
            let mut snapshot = sample();
            snapshot.processes[0].start_time = u64::MAX;
            snapshot
        };
        let found = port_owners(8080, &runner, reused).unwrap();
        assert_eq!(found.owners[0].refusal, Some(Refusal::IdentityUncertain));
        assert!(!found.owners[0].actionable());

        let runner = MockRunner::new().with_output(
            LSOF,
            &LISTENER_ARGS,
            Ok(outcome(0, "p4100\nf5\nn*:8080\n", "")),
        );
        let found = listeners(&runner, reused).unwrap();
        assert_eq!(found.listeners[0].refusal, Some(Refusal::IdentityUncertain));
        assert!(!found.listeners[0].actionable);
    }

    #[test]
    fn no_visible_owner_keeps_the_caveat() {
        let found = owners_with(Ok(outcome(1, "", ""))).unwrap();
        assert!(found.owners.is_empty());
        assert!(found
            .visibility_caveat
            .contains("does not mean the port is free"));
    }

    #[test]
    fn failed_or_timed_out_owner_probe_is_an_error() {
        let failed = owners_with(Ok(outcome(1, "", "lsof: unsupported")));
        assert!(matches!(failed, Err(Error::Operation(d)) if d.contains("unsupported")));
        let timeout = CommandError::Timeout {
            bin: LSOF.into(),
            timeout_ms: 3000,
        };
        let timed_out = owners_with(Err(timeout));
        assert!(matches!(timed_out, Err(Error::Operation(d)) if d.contains("did not finish")));
    }

    #[test]
    fn port_zero_is_rejected_without_probing() {
        let runner = MockRunner::new();
        let result = port_owners(0, &runner, || panic!("must not sample"));
        assert!(matches!(result, Err(Error::InvalidInput(_))));
        assert!(runner.output_calls.lock().unwrap().is_empty());
    }

    fn listeners_with(
        result: std::result::Result<CommandOutcome, CommandError>,
    ) -> Result<Listeners> {
        let runner = MockRunner::new().with_output(LSOF, &LISTENER_ARGS, result);
        listeners(&runner, sample)
    }

    #[test]
    fn listeners_are_sorted_deduped_and_keep_unsampled_owners() {
        // 4100 listens on IPv4 and IPv6 for 5173 (with a duplicate fd), and on 3000.
        let stdout = "p4200\nf4\nn*:8080\n\
            p4100\nf5\nn127.0.0.1:5173\nf6\nn[::1]:5173\nf7\nn127.0.0.1:5173\nf8\nn*:3000\n\
            p4300\nf3\nn*:3000\n";
        let found = listeners_with(Ok(outcome(0, stdout, ""))).unwrap();
        let summary: Vec<_> = found
            .listeners
            .iter()
            .map(|l| (l.port, l.pid, l.address.as_str(), l.actionable))
            .collect();
        assert_eq!(
            summary,
            vec![
                (3000, 4100, "*", true),
                (3000, 4300, "*", false),
                (5173, 4100, "127.0.0.1", true),
                (5173, 4100, "[::1]", true),
                (8080, 4200, "*", false),
            ]
        );
        assert_eq!(found.listeners[1].process, None);
        assert_eq!(found.listeners[1].refusal, None);
        assert_eq!(found.listeners[4].refusal, Some(Refusal::OtherUser));
        assert_eq!(
            found.listeners[0].process.as_ref().map(|p| p.pid),
            Some(4100)
        );
        assert_eq!(found.sampled_at, 7);
        assert_eq!(found.visibility_caveat, PORT_VISIBILITY_CAVEAT);
    }

    #[test]
    fn no_visible_listener_still_carries_the_caveat() {
        let found = listeners_with(Ok(outcome(1, "", ""))).unwrap();
        assert!(found.listeners.is_empty());
        assert_eq!(found.visibility_caveat, PORT_VISIBILITY_CAVEAT);
    }

    #[test]
    fn failed_or_timed_out_listener_probe_is_an_error() {
        let failed = listeners_with(Ok(outcome(2, "", "lsof: bad option")));
        assert!(matches!(failed, Err(Error::Operation(d)) if d.contains("bad option")));
        let timeout = CommandError::Timeout {
            bin: LSOF.into(),
            timeout_ms: 3000,
        };
        assert!(matches!(
            listeners_with(Err(timeout)),
            Err(Error::Operation(d)) if d.contains("did not finish")
        ));
    }
}
