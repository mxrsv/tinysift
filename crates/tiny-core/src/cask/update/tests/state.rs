use super::*;

#[test]
fn an_open_app_is_running_and_a_closed_one_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let app = installed_app(dir.path(), "Ghostty");
    let state = |running: &[&str]| {
        run_state(
            std::slice::from_ref(&app),
            &MockChecker::with_running(running.iter().copied()),
            &executable,
        )
    };
    assert_eq!(
        state(&["GhosttyExec"]),
        RunState::Running {
            app: "Ghostty".into()
        }
    );
    assert_eq!(state(&[]), RunState::NotRunning);
}

#[test]
fn a_check_that_cannot_tell_is_not_taken_for_not_running() {
    let dir = tempfile::tempdir().unwrap();
    let app = installed_app(dir.path(), "Ghostty");
    let failed = run_state(std::slice::from_ref(&app), &FailingProbe, &executable);
    assert!(matches!(failed, RunState::Unknown { app, .. } if app == "Ghostty"));
    let nameless = run_state(&[app], &MockChecker::none(), &|_| None);
    assert!(matches!(nameless, RunState::Unknown { error, .. } if error.contains("process name")));
}

#[test]
fn an_app_that_is_not_there_has_nothing_running_and_an_open_one_wins() {
    let dir = tempfile::tempdir().unwrap();
    let gone = dir.path().join("Gone.app");
    let open = installed_app(dir.path(), "Open");
    let odd = installed_app(dir.path(), "Odd");
    assert_eq!(
        run_state(std::slice::from_ref(&gone), &FailingProbe, &executable),
        RunState::NotRunning
    );
    let probe = MockChecker::with_running(["OpenExec"]);
    let no_name_for_odd = |p: &Path| (p != odd).then(|| executable(p)).flatten();
    assert_eq!(
        run_state(&[odd.clone(), gone, open], &probe, &no_name_for_odd),
        RunState::Running { app: "Open".into() }
    );
}
