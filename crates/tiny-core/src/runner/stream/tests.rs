use super::*;
use crate::runner::test_support::MockRunner;
use crate::runner::{CommandRunner, ToolLookup};

const SH: &str = "/bin/sh";
const GENEROUS: Duration = Duration::from_secs(10);

fn runner() -> ToolRunner {
    ToolRunner::new(ToolLookup::default())
}

/// Runs `script` under `/bin/sh`, collecting every line the callback sees.
fn stream(
    script: &str,
    timeout: Duration,
    cancel: Option<&AtomicBool>,
    grace: Duration,
) -> (Result<StreamEnd, CommandError>, Vec<(Duration, String)>) {
    let started = Instant::now();
    let mut seen = Vec::new();
    let result = runner().run_stream(SH, &["-c", script], timeout, cancel, grace, &mut |line| {
        seen.push((started.elapsed(), line.to_string()))
    });
    (result, seen)
}

fn finished(result: Result<StreamEnd, CommandError>) -> CommandOutcome {
    match result {
        Ok(StreamEnd::Finished(outcome)) => outcome,
        other => panic!("expected a finished command, got {other:?}"),
    }
}

fn texts(seen: &[(Duration, String)]) -> Vec<&str> {
    seen.iter().map(|(_, line)| line.as_str()).collect()
}

/// Whether `pid` is gone, giving the kernel a moment to reap it.
fn gone(pid: libc::pid_t) -> bool {
    (0..40).any(|_| {
        // SAFETY: signal 0 only asks whether the process exists.
        let alive = unsafe { libc::kill(pid, 0) } == 0;
        if alive {
            thread::sleep(Duration::from_millis(50));
        }
        !alive
    })
}

#[test]
fn lines_reach_the_callback_while_the_command_still_runs() {
    let (result, seen) = stream(
        "echo '==> first'; sleep 1; echo '==> second'; echo oops >&2",
        GENEROUS,
        None,
        INTERRUPT_GRACE,
    );
    let outcome = finished(result);
    assert_eq!(outcome.status, Some(0));
    assert_eq!(outcome.stdout, "==> first\n==> second\n");
    assert_eq!(outcome.stderr, "oops\n");
    assert_eq!(texts(&seen).len(), 3);
    let first = seen.iter().find(|(_, line)| line == "==> first").unwrap().0;
    let second = seen
        .iter()
        .find(|(_, line)| line == "==> second")
        .unwrap()
        .0;
    assert!(
        first < Duration::from_millis(700),
        "first line came at {first:?}"
    );
    assert!(
        second >= Duration::from_millis(900),
        "second line came at {second:?}"
    );
}

#[test]
fn stderr_lines_are_handed_out_and_its_end_is_kept() {
    let script = "i=0; while [ $i -lt 3000 ]; do echo 'noise noise noise noise' >&2; \
                  i=$((i+1)); done; echo 'Error: the last word' >&2; exit 4";
    let (result, seen) = stream(script, GENEROUS, None, INTERRUPT_GRACE);
    let outcome = finished(result);
    assert_eq!(outcome.status, Some(4));
    assert_eq!(seen.len(), 3001);
    assert!(outcome.stderr.len() <= STDERR_KEPT);
    assert!(outcome.stderr.ends_with("Error: the last word\n"));
}

#[test]
fn a_flood_of_stdout_arrives_in_order_and_is_kept_from_its_start() {
    let (result, seen) = stream("seq 1 20000", GENEROUS, None, INTERRUPT_GRACE);
    let outcome = finished(result);
    assert_eq!(seen.len(), 20000);
    assert!(texts(&seen)
        .iter()
        .enumerate()
        .all(|(i, line)| *line == (i + 1).to_string()));
    assert_eq!(outcome.stdout.len(), STDOUT_KEPT);
    assert!(outcome.stdout.starts_with("1\n2\n3\n"));
}

#[test]
fn text_that_is_not_utf8_does_not_fail_the_run() {
    let (result, seen) = stream("printf 'caf\\377\\n'", GENEROUS, None, INTERRUPT_GRACE);
    assert!(finished(result).success());
    assert_eq!(texts(&seen), ["caf\u{fffd}"]);
}

#[test]
fn cancelling_interrupts_the_command_without_waiting_out_the_grace() {
    let cancel = AtomicBool::new(false);
    let started = Instant::now();
    let mut on_line = |line: &str| {
        if line == "started" {
            cancel.store(true, Ordering::Release);
        }
    };
    let result = runner().run_stream(
        SH,
        &["-c", "echo started; exec sleep 30"],
        GENEROUS,
        Some(&cancel),
        INTERRUPT_GRACE,
        &mut on_line,
    );
    assert_eq!(result, Ok(StreamEnd::Cancelled));
    assert!(
        started.elapsed() < Duration::from_secs(3),
        "{:?}",
        started.elapsed()
    );
}

#[test]
fn a_command_that_ignores_the_interrupt_is_killed_with_what_it_started() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("child.pid");
    let script = format!(
        "trap '' INT; sleep 30 & echo $! > '{}'; echo started; wait",
        pidfile.display()
    );
    let cancel = AtomicBool::new(false);
    let started = Instant::now();
    let grace = Duration::from_millis(400);
    let mut on_line = |line: &str| {
        if line == "started" {
            cancel.store(true, Ordering::Release);
        }
    };
    let result = runner().run_stream(
        SH,
        &["-c", &script],
        GENEROUS,
        Some(&cancel),
        grace,
        &mut on_line,
    );
    assert_eq!(result, Ok(StreamEnd::Cancelled));
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
    let killed = gone(child);
    if !killed {
        // SAFETY: cleanup of the stray process this test started.
        unsafe { libc::kill(child, libc::SIGKILL) };
    }
    assert!(killed, "what the command started outlived the kill");
}

#[test]
fn a_command_that_runs_too_long_is_stopped_and_reported() {
    let started = Instant::now();
    let (result, seen) = stream(
        "echo a; sleep 30",
        Duration::from_millis(300),
        None,
        INTERRUPT_GRACE,
    );
    assert!(matches!(
        result,
        Err(CommandError::Timeout {
            timeout_ms: 300,
            ..
        })
    ));
    assert_eq!(texts(&seen), ["a"]);
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
fn a_flag_raised_before_the_start_spawns_nothing() {
    let cancel = AtomicBool::new(true);
    let result = runner().run_stream(
        "/nonexistent/tiny-test-binary",
        &[],
        GENEROUS,
        Some(&cancel),
        INTERRUPT_GRACE,
        &mut |_| panic!("no line without a process"),
    );
    assert_eq!(result, Ok(StreamEnd::Cancelled));
}

#[test]
fn a_missing_binary_is_reported() {
    let result = runner().run_stream(
        "/nonexistent/tiny-test-binary",
        &[],
        GENEROUS,
        None,
        INTERRUPT_GRACE,
        &mut |_| {},
    );
    assert!(matches!(result, Err(CommandError::NotFound(_))));
}

#[test]
fn a_runner_that_cannot_stream_replays_its_output_after_the_run() {
    let args = ["install", "--cask", "x"];
    let mock = MockRunner::new().with_output(
        "brew",
        &args,
        Ok(CommandOutcome {
            status: Some(0),
            stdout: "==> one\n==> two\n".into(),
            stderr: "==> heading\n".into(),
        }),
    );
    let mut seen = Vec::new();
    let end = mock.stream("brew", &args, GENEROUS, None, &mut |line| {
        seen.push(line.to_string())
    });
    assert!(matches!(end, Ok(StreamEnd::Finished(outcome)) if outcome.success()));
    assert_eq!(seen, ["==> heading", "==> one", "==> two"]);

    let cancel = AtomicBool::new(true);
    let before = mock.output_calls.lock().unwrap().len();
    let end = mock.stream("brew", &args, GENEROUS, Some(&cancel), &mut |_| {});
    assert_eq!(end, Ok(StreamEnd::Cancelled));
    assert_eq!(
        mock.output_calls.lock().unwrap().len(),
        before,
        "no second run"
    );
}

#[test]
fn timeout_stops_inherited_pipe_child_after_leader_exits() {
    let fixture = crate::runner::tests::InheritedPipeFixture::new();
    let args = fixture.args(false, false);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let started = Instant::now();
    let result = runner().run_stream(
        SH,
        &args,
        Duration::from_millis(150),
        None,
        Duration::from_millis(100),
        &mut |_| {},
    );
    let elapsed = started.elapsed();
    let survived = fixture.survived();
    assert!(
        matches!(result, Err(CommandError::Timeout { .. })),
        "{result:?}"
    );
    assert!(
        !survived,
        "descendant outlived streamed timeout after leader exit"
    );
    assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
}

#[test]
fn cancellation_grace_includes_descendant_after_leader_handles_interrupt() {
    let fixture = crate::runner::tests::InheritedPipeFixture::new();
    let args = fixture.args(true, false);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let cancel = AtomicBool::new(false);
    let started = Instant::now();
    let grace = Duration::from_millis(150);
    let result = runner().run_stream(SH, &args, GENEROUS, Some(&cancel), grace, &mut |line| {
        if line == "started" {
            cancel.store(true, Ordering::Release);
        }
    });
    let elapsed = started.elapsed();
    let survived = fixture.survived();
    assert_eq!(result, Ok(StreamEnd::Cancelled));
    assert!(
        !survived,
        "SIGINT-exited leader left descendant ignoring SIGINT"
    );
    assert!(
        elapsed >= grace && elapsed < Duration::from_secs(2),
        "{elapsed:?}"
    );
}

#[test]
fn continuous_inherited_output_cannot_bypass_deadline_or_cancel() {
    for cancelling in [false, true] {
        let fixture = crate::runner::tests::InheritedPipeFixture::new();
        let args = fixture.args(false, true);
        let args: Vec<&str> = args.iter().map(String::as_str).collect();
        let cancel = AtomicBool::new(false);
        let started = Instant::now();
        let mut seen = 0;
        let result = runner().run_stream(
            SH,
            &args,
            Duration::from_millis(250),
            Some(&cancel),
            Duration::from_millis(100),
            &mut |_| {
                seen += 1;
                if cancelling && started.elapsed() >= Duration::from_millis(100) {
                    cancel.store(true, Ordering::Release);
                }
            },
        );
        let elapsed = started.elapsed();
        let survived = fixture.survived();
        assert!(seen > 0, "fixture must keep the inherited pipe active");
        if cancelling {
            assert_eq!(result, Ok(StreamEnd::Cancelled));
        } else {
            assert!(
                matches!(result, Err(CommandError::Timeout { .. })),
                "{result:?}"
            );
        }
        assert!(!survived, "continuous writer outlived bounded stream");
        assert!(elapsed < Duration::from_secs(2), "{elapsed:?}");
    }
}

#[test]
fn callback_panic_stops_owned_group_and_reaps_leader() {
    let fixture = crate::runner::tests::InheritedPipeFixture::new();
    let args = fixture.args(true, false);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        runner().run_stream(SH, &args, GENEROUS, None, INTERRUPT_GRACE, &mut |line| {
            if line == "started" {
                panic!("disposable callback panic");
            }
        })
    }));
    let survived = fixture.survived();
    assert!(result.is_err());
    assert!(!survived, "callback unwind left owned descendant running");
}

#[test]
fn deadline_after_last_callback_keeps_timeout_when_group_already_exited() {
    let result = runner().run_stream(
        SH,
        &["-c", "echo done"],
        Duration::from_millis(50),
        None,
        Duration::from_millis(100),
        &mut |_| thread::sleep(Duration::from_millis(150)),
    );
    assert!(
        matches!(result, Err(CommandError::Timeout { timeout_ms: 50, .. })),
        "{result:?}"
    );
}
