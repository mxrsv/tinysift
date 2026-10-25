use std::ffi::OsString;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::runner::{CommandError, CommandRunner, StreamEnd, ToolLookup, ToolRunner};

const GENEROUS: Duration = Duration::from_secs(10);
const ISOLATED_PATH: &str = "/usr/bin:/bin";

fn lookup(home: Option<&Path>) -> ToolLookup {
    ToolLookup::new(Some(OsString::from("/usr/bin:/bin")), Vec::new())
        .with_env(&[("HOMEBREW_NO_SUDO", "1"), ("CI", "1")])
        .isolated(home.map(Path::to_path_buf), ISOLATED_PATH)
}

fn env_of(lookup: ToolLookup) -> Vec<String> {
    let outcome = ToolRunner::new(lookup)
        .output("env", &[], GENEROUS)
        .expect("env runs");
    let mut lines: Vec<String> = outcome.stdout.lines().map(String::from).collect();
    lines.sort();
    lines
}

#[test]
fn the_tool_gets_home_a_fixed_path_and_the_lookups_variables_and_nothing_else() {
    let home = Path::new("/Users/someone");
    let env = env_of(lookup(Some(home)));
    // `env` itself may add `_` (the command) depending on the shell that
    // started it; everything else is exactly what was set.
    let named: Vec<&str> = env
        .iter()
        .map(String::as_str)
        .filter(|line| !line.starts_with("_="))
        .collect();
    assert_eq!(
        named,
        [
            "CI=1",
            "HOME=/Users/someone",
            "HOMEBREW_NO_SUDO=1",
            "PATH=/usr/bin:/bin"
        ]
    );
    // The test process has plenty of its own.
    assert!(std::env::var_os("CARGO_MANIFEST_DIR").is_some());
}

#[test]
fn the_home_is_the_one_given_and_without_one_the_tool_does_not_run() {
    let result = ToolRunner::new(lookup(None)).output("env", &[], GENEROUS);
    assert!(
        matches!(result, Err(CommandError::NotFound(_))),
        "{result:?}"
    );
    assert!(ToolRunner::new(lookup(None))
        .stream("env", &[], GENEROUS, None, &mut |_| {})
        .is_err());
}

/// The session of the process whose id a script prints first.
fn session_of_streamed_script(runner: &ToolRunner, script: &str) -> (libc::pid_t, libc::pid_t) {
    let mut seen = None;
    let end = runner.stream("/bin/sh", &["-c", script], GENEROUS, None, &mut |line| {
        if let (None, Ok(pid)) = (seen, line.trim().parse::<libc::pid_t>()) {
            // SAFETY: getsid only reads the process table.
            seen = Some((pid, unsafe { libc::getsid(pid) }));
        }
    });
    assert!(matches!(end, Ok(StreamEnd::Finished(_))), "{end:?}");
    seen.expect("the script printed its pid")
}

#[test]
fn an_isolated_tool_leads_a_session_of_its_own_and_others_stay_in_ours() {
    let script = "echo $$; sleep 0.5";
    let (pid, session) =
        session_of_streamed_script(&ToolRunner::new(lookup(Some(Path::new("/tmp")))), script);
    assert_eq!(session, pid, "the tool is its session's leader");

    let (_, shared) = session_of_streamed_script(&ToolRunner::new(ToolLookup::default()), script);
    // SAFETY: getsid(0) is this process's own session.
    assert_eq!(shared, unsafe { libc::getsid(0) });
}

#[test]
fn cancelling_an_isolated_tool_stops_what_it_started_with_the_kill_after_the_grace() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("child.pid");
    let script = format!(
        "trap '' INT; sleep 30 & echo $! > '{}'; echo started; wait",
        pidfile.display()
    );
    let cancel = AtomicBool::new(false);
    let started = Instant::now();
    // Nothing here dies of SIGINT, so the kill after the grace period must
    // reach the leader and what it started through the tool's process group.
    let grace = Duration::from_millis(300);
    let result = ToolRunner::new(lookup(Some(dir.path()))).run_stream(
        "/bin/sh",
        &["-c", &script],
        GENEROUS,
        Some(&cancel),
        grace,
        &mut |line| {
            if line == "started" {
                cancel.store(true, Ordering::Release);
            }
        },
    );
    assert!(matches!(result, Ok(StreamEnd::Cancelled)), "{result:?}");
    let elapsed = started.elapsed();
    assert!(
        elapsed >= grace && elapsed < Duration::from_secs(3),
        "{elapsed:?}"
    );
    let child: libc::pid_t = std::fs::read_to_string(&pidfile)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let gone = (0..40).any(|_| {
        // SAFETY: signal 0 only asks whether the process exists.
        let alive = unsafe { libc::kill(child, 0) } == 0;
        if alive {
            std::thread::sleep(Duration::from_millis(50));
        }
        !alive
    });
    if !gone {
        // SAFETY: cleanup of the stray process this test started.
        unsafe { libc::kill(child, libc::SIGKILL) };
    }
    assert!(gone, "what the tool started outlived the cancel");
}

#[test]
fn a_timeout_ends_an_isolated_tool_and_what_it_started() {
    let started = Instant::now();
    let result = ToolRunner::new(lookup(Some(Path::new("/tmp")))).output(
        "/bin/sh",
        &["-c", "sleep 30 & wait"],
        Duration::from_millis(300),
    );
    assert!(
        matches!(result, Err(CommandError::Timeout { .. })),
        "{result:?}"
    );
    assert!(started.elapsed() < Duration::from_secs(3));
}
