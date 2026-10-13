//! End-to-end checks for `tiny processes`. Signals only reach disposable
//! children spawned here; refusal checks assert the reason so a missing TTY
//! cannot make them pass by accident.

use std::os::unix::process::ExitStatusExt;
use std::process::Child;

use assert_cmd::Command;
use predicates::prelude::*;
use serde_json::Value;

const SIGTERM: i32 = 15;
const SIGKILL: i32 = 9;

/// Kills and reaps the child even when an assertion fails first.
struct Disposable(Child);

impl Drop for Disposable {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn sleeper() -> Disposable {
    Disposable(
        std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap(),
    )
}

fn tiny() -> Command {
    Command::cargo_bin("tiny").unwrap()
}

fn json_stdout(args: &[&str]) -> Value {
    let output = tiny().args(args).assert().success().get_output().clone();
    serde_json::from_slice(&output.stdout).unwrap()
}

#[test]
fn list_prints_a_table() {
    tiny()
        .args(["processes", "--limit", "5"])
        .assert()
        .success()
        .stdout(predicate::str::contains("PID").and(predicate::str::contains("Sampled at")));
}

#[test]
fn list_json_is_measured_and_limited() {
    let report = json_stdout(&["processes", "--json", "--limit", "3", "--sort", "memory"]);
    assert_eq!(report["cpuMeasured"], Value::Bool(true));
    assert!(report["sampledAt"].as_u64().is_some());
    let processes = report["processes"].as_array().unwrap();
    assert!(!processes.is_empty() && processes.len() <= 3);
    for key in [
        "pid",
        "name",
        "isCurrentUser",
        "startTime",
        "cpuMeasured",
        "memoryBytes",
    ] {
        assert!(processes[0].get(key).is_some(), "missing {key}");
    }
}

#[test]
fn show_reports_a_disposable_child() {
    let child = sleeper();
    let pid = child.0.id();
    let report = json_stdout(&["processes", "show", &pid.to_string(), "--json"]);
    assert_eq!(report["pid"].as_u64(), Some(u64::from(pid)));
    assert_eq!(report["name"], "sleep");
    assert_eq!(
        report["parentPid"].as_u64(),
        Some(u64::from(std::process::id()))
    );
    assert!(report["children"].as_array().unwrap().is_empty());
    assert!(report["ports"].is_array() || report["portsError"].is_string());
    assert!(report["sampledAt"].as_u64().is_some());
}

#[test]
fn quit_sends_sigterm_to_a_disposable_child() {
    let mut child = sleeper();
    let pid = child.0.id().to_string();
    tiny()
        .args(["processes", "quit", &pid, "--yes"])
        .assert()
        .success()
        .stdout(predicate::str::contains("exited after SIGTERM"));
    assert_eq!(child.0.wait().unwrap().signal(), Some(SIGTERM));
}

#[test]
fn quit_without_yes_and_without_a_tty_signals_nothing() {
    let mut child = sleeper();
    let pid = child.0.id().to_string();
    for extra in [&[][..], &["--force"][..]] {
        tiny()
            .args(["processes", "quit", &pid])
            .args(extra)
            .env("TINY_CONFIRM_FORCE", "1")
            .assert()
            .failure()
            .stderr(predicate::str::contains("quit confirm prompt failed"));
        assert!(
            child.0.try_wait().unwrap().is_none(),
            "child must survive {extra:?}"
        );
    }
}

#[test]
fn force_yes_needs_the_env_then_sends_sigkill() {
    let mut child = sleeper();
    let pid = child.0.id().to_string();
    tiny()
        .args(["processes", "quit", &pid, "--force", "--yes"])
        .env_remove("TINY_CONFIRM_FORCE")
        .assert()
        .failure()
        .stderr(predicate::str::contains("TINY_CONFIRM_FORCE=1"));
    assert!(child.0.try_wait().unwrap().is_none(), "child must survive");
    tiny()
        .args(["processes", "quit", &pid, "--force", "--yes"])
        .env("TINY_CONFIRM_FORCE", "1")
        .assert()
        .success()
        .stdout(predicate::str::contains("exited after SIGKILL"));
    assert_eq!(child.0.wait().unwrap().signal(), Some(SIGKILL));
}

#[test]
fn launchd_is_refused() {
    tiny()
        .args(["processes", "quit", "1", "--yes"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("system process"));
}

#[test]
fn tiny_refuses_to_quit_itself() {
    // `exec` keeps the shell's PID, so `$$` is the PID of `tiny` itself.
    let tiny_bin = assert_cmd::cargo::cargo_bin("tiny");
    let output = std::process::Command::new("/bin/sh")
        .args(["-c", r#"exec "$0" processes quit $$ --yes"#])
        .arg(tiny_bin)
        .output()
        .unwrap();
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("it is this process"), "stderr: {stderr}");
}

#[test]
fn tiny_refuses_to_quit_its_parent() {
    // No --yes: even a broken refusal would stop at the prompt without a TTY.
    tiny()
        .args(["processes", "quit", &std::process::id().to_string()])
        .assert()
        .failure()
        .stderr(predicate::str::contains("it is the parent of this process"));
}

#[test]
#[cfg(target_os = "macos")]
fn port_filter_lists_a_disposable_listener_with_the_caveat() {
    use std::io::{BufRead, BufReader};
    use std::process::Stdio;

    let script = "import socket,time\ns=socket.socket()\ns.bind(('127.0.0.1',0))\ns.listen()\nprint(s.getsockname()[1],flush=True)\ntime.sleep(30)";
    let Ok(spawned) = std::process::Command::new("python3")
        .args(["-c", script])
        .stdout(Stdio::piped())
        .spawn()
    else {
        eprintln!("python3 unavailable; --port is covered by core fixtures only");
        return;
    };
    let mut child = Disposable(spawned);
    let mut line = String::new();
    BufReader::new(child.0.stdout.take().unwrap())
        .read_line(&mut line)
        .unwrap();
    let port = line.trim().to_string();
    let pid = u64::from(child.0.id());

    let report = json_stdout(&["processes", "--port", &port, "--json"]);
    let owner = report["portOwners"]
        .as_array()
        .unwrap()
        .iter()
        .find(|owner| owner["pid"].as_u64() == Some(pid))
        .expect("listener is a visible owner");
    assert_eq!(owner["actionable"], Value::Bool(true));
    assert!(report["processes"]
        .as_array()
        .unwrap()
        .iter()
        .any(|p| p["pid"].as_u64() == Some(pid)));
    assert!(report["visibilityCaveat"]
        .as_str()
        .unwrap()
        .contains("does not mean the port is free"));

    tiny()
        .args(["processes", "--port", &port])
        .assert()
        .success()
        .stdout(predicate::str::contains(pid.to_string()).and(predicate::str::contains("Note:")));
}

#[test]
fn port_zero_is_rejected() {
    tiny()
        .args(["processes", "--port", "0"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("port must be between"));
}
