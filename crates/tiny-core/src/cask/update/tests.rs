use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use super::*;
use crate::cask::test_support::*;
use crate::cask::Stage;
use crate::clean::process::test_support::{FailingProbe, MockChecker};
use crate::runner::test_support::MockRunner;

fn brew(runner: &MockRunner) -> Brew<'_> {
    Brew::new(Path::new(BREW_PATH), runner)
}

/// The process name of `X.app` in these tests is `XExec`.
fn executable(path: &Path) -> Option<String> {
    Some(format!("{}Exec", path.file_stem()?.to_str()?))
}

fn installed_app(dir: &Path, name: &str) -> PathBuf {
    let app = dir.join(format!("{name}.app"));
    std::fs::create_dir(&app).unwrap();
    app
}

fn listed(token: &str) -> OutdatedCask {
    OutdatedCask {
        token: token.into(),
        name: token.into(),
        from_version: "1.0".into(),
        version: "2.0".into(),
        download_host: None,
        blocked: None,
        apps: Vec::new(),
    }
}

// ---- run state

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

// ---- planning

fn outdated_info(token: &str, outdated: bool) -> serde_json::Value {
    let mut info = simple_cask(token, "2.0", &format!("{token}.app"));
    info["installed"] = serde_json::json!("1.0");
    info["outdated"] = serde_json::json!(outdated);
    info
}

#[test]
fn a_plan_is_read_again_from_brew_and_keeps_the_order_asked() {
    let runner = MockRunner::new().with_exit(
        BREW_PATH,
        &["info", "--cask", "--json=v2", "b", "a"],
        0,
        &info_json(&[outdated_info("a", true), outdated_info("b", true)]),
    );
    let items = plan_updates(
        &brew(&runner),
        &[listed("a"), listed("b"), listed("c")],
        &["b".into(), "a".into()],
    )
    .unwrap();
    let seen: Vec<_> = items
        .iter()
        .map(|i| {
            (
                i.token.as_str(),
                i.action,
                i.from_version.as_deref(),
                i.version.as_str(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            ("b", CaskAction::Update, Some("1.0"), "2.0"),
            ("a", CaskAction::Update, Some("1.0"), "2.0")
        ]
    );
    assert_eq!(items[0].command(), "brew upgrade --cask b");
}

#[test]
fn a_token_outside_the_listing_or_repeated_never_reaches_brew() {
    let runner = MockRunner::new();
    let plan = |tokens: &[&str]| {
        let tokens: Vec<String> = tokens.iter().map(|t| t.to_string()).collect();
        plan_updates(&brew(&runner), &[listed("a"), listed("b")], &tokens)
    };
    for tokens in [
        &["z"][..],
        &["a", "a"],
        &["--greedy"],
        &["a", "user/tap/a"],
        &[""],
    ] {
        assert!(
            matches!(plan(tokens), Err(CaskFailure::NotOutdated { .. })),
            "{tokens:?}"
        );
    }
    assert_eq!(plan(&[]), Ok(Vec::new()));
    assert!(runner.output_calls.lock().unwrap().is_empty());
}

#[test]
fn a_cask_brew_no_longer_calls_outdated_is_refused() {
    let runner = MockRunner::new().with_exit(
        BREW_PATH,
        &["info", "--cask", "--json=v2", "a"],
        0,
        &info_json(&[outdated_info("a", false)]),
    );
    let refused = plan_updates(&brew(&runner), &[listed("a")], &["a".into()]);
    assert_eq!(refused, Err(CaskFailure::NotOutdated { token: "a".into() }));
}

#[test]
fn an_update_needing_a_password_is_planned_blocked_and_a_launchctl_one_is_not() {
    let plan = |artifacts: serde_json::Value, version: &str| {
        let mut info = cask("a", version, artifacts);
        info["installed"] = serde_json::json!("1.0");
        info["outdated"] = serde_json::json!(true);
        let runner = MockRunner::new().with_exit(
            BREW_PATH,
            &["info", "--cask", "--json=v2", "a"],
            0,
            &info_json(&[info]),
        );
        plan_updates(&brew(&runner), &[listed("a")], &["a".into()]).unwrap()
    };
    let app = serde_json::json!({"app": ["A.app"], "target": "/Applications/A.app"});
    let soft = plan(
        serde_json::json!([{"uninstall": [{"launchctl": "com.example.helper", "quit": "com.example.a"}]}, app]),
        "2.0,77",
    );
    assert_eq!(soft[0].blocked, None, "launchctl tries sudo and carries on");
    assert_eq!(
        soft[0].version, "2.0",
        "no build number in the shown version"
    );
    let hard = plan(
        serde_json::json!([{"uninstall": [{"pkgutil": "com.example.pkg"}]}, app]),
        "2.0",
    );
    assert_eq!(hard[0].blocked, Some(CaskFailure::NeedsAdministrator));
}

#[test]
fn a_cask_whose_app_already_is_the_new_version_is_not_planned() {
    let mut info = outdated_info("a", true);
    info["bundle_short_version"] = serde_json::json!("2.0");
    let runner = MockRunner::new().with_exit(
        BREW_PATH,
        &["info", "--cask", "--json=v2", "a"],
        0,
        &info_json(&[info]),
    );
    let refused = plan_updates(&brew(&runner), &[listed("a")], &["a".into()]);
    assert_eq!(refused, Err(CaskFailure::NotOutdated { token: "a".into() }));
}

// ---- running

fn item(dir: &Path, token: &str) -> CaskPlanItem {
    plan_item(CaskAction::Update, token, installed_app(dir, token))
}

fn outdated_call() -> String {
    format!("{BREW_PATH} outdated --cask --json=v2")
}

fn upgrade(token: &str) -> String {
    format!("{BREW_PATH} upgrade --cask {token}")
}

/// A Homebrew that lists `tokens` as outdated and upgrades each one.
fn runner_for(tokens: &[&str]) -> MockRunner {
    let listing = outdated_json(
        &tokens
            .iter()
            .map(|token| (*token, "1.0", "2.0", false))
            .collect::<Vec<_>>(),
    );
    tokens.iter().fold(
        MockRunner::new().with_exit(BREW_PATH, &["outdated", "--cask", "--json=v2"], 0, &listing),
        |runner, token| {
            runner.with_exit(
                BREW_PATH,
                &["upgrade", "--cask", token],
                0,
                "==> Upgrading x\n==> Moving App 'x.app'\n",
            )
        },
    )
}

fn calls(runner: &MockRunner) -> Vec<String> {
    runner.output_calls.lock().unwrap().clone()
}

#[test]
fn casks_are_upgraded_one_at_a_time_in_order_with_progress_naming_the_token() {
    let dir = tempfile::tempdir().unwrap();
    let items = [item(dir.path(), "a"), item(dir.path(), "b")];
    let runner = runner_for(&["a", "b"]);
    let seen = Mutex::new(Vec::<CaskProgress>::new());
    let report = |p: CaskProgress| seen.lock().unwrap().push(p);
    let results = run_updates(
        &brew(&runner),
        &items,
        &MockChecker::none(),
        &executable,
        None,
        Some(&report),
    );
    assert_eq!(
        results,
        [
            ("a".to_string(), RunOutcome::Done),
            ("b".to_string(), RunOutcome::Done)
        ]
    );
    assert_eq!(
        calls(&runner),
        [outdated_call(), upgrade("a"), outdated_call(), upgrade("b")]
    );
    let steps: Vec<_> = seen
        .into_inner()
        .unwrap()
        .into_iter()
        .map(|p| (p.token, p.position, p.total, p.stage))
        .collect();
    let step = |token: &str, position, stage| (token.to_string(), position, 2, stage);
    assert_eq!(
        steps,
        [
            step("a", 0, Stage::Working),
            step("a", 0, Stage::Installing),
            step("b", 1, Stage::Working),
            step("b", 1, Stage::Installing),
        ]
    );
}

#[test]
fn a_cask_that_updated_itself_since_the_preview_is_not_upgraded() {
    // `brew upgrade --cask b` would act greedily and reinstall it, so the
    // default list is read first and `b` has left it.
    let dir = tempfile::tempdir().unwrap();
    let items = [item(dir.path(), "a"), item(dir.path(), "b")];
    let runner = runner_for(&["a"]);
    let results = run_updates(
        &brew(&runner),
        &items,
        &MockChecker::none(),
        &executable,
        None,
        None,
    );
    assert_eq!(results[0].1, RunOutcome::Done);
    assert_eq!(
        results[1].1,
        RunOutcome::Failed(CaskFailure::NotOutdated { token: "b".into() })
    );
    assert!(!calls(&runner).contains(&upgrade("b")));
}

#[test]
fn a_failing_outdated_check_is_not_taken_for_still_outdated() {
    let dir = tempfile::tempdir().unwrap();
    let items = [item(dir.path(), "a")];
    let runner =
        MockRunner::new().with_exit(BREW_PATH, &["outdated", "--cask", "--json=v2"], 1, "");
    let results = run_updates(
        &brew(&runner),
        &items,
        &MockChecker::none(),
        &executable,
        None,
        None,
    );
    assert!(matches!(
        results[0].1,
        RunOutcome::Failed(CaskFailure::BrewFailed { .. })
    ));
    assert_eq!(calls(&runner), [outdated_call()]);
}

#[test]
fn an_open_app_is_refused_without_spawning_and_the_others_go_on() {
    let dir = tempfile::tempdir().unwrap();
    let items = [
        item(dir.path(), "a"),
        item(dir.path(), "b"),
        item(dir.path(), "c"),
    ];
    let runner = runner_for(&["a", "b", "c"]);
    let probe = MockChecker::with_running(["bExec"]);
    let results = run_updates(&brew(&runner), &items, &probe, &executable, None, None);
    assert_eq!(
        results[1],
        (
            "b".to_string(),
            RunOutcome::Failed(CaskFailure::AppRunning { app: "b".into() })
        )
    );
    assert_eq!(
        (&results[0].1, &results[2].1),
        (&RunOutcome::Done, &RunOutcome::Done)
    );
    assert!(!calls(&runner).contains(&upgrade("b")));
}

#[test]
fn a_check_that_fails_refuses_the_update_instead_of_assuming_it_is_closed() {
    let dir = tempfile::tempdir().unwrap();
    let items = [item(dir.path(), "a")];
    let runner = runner_for(&["a"]);
    let results = run_updates(
        &brew(&runner),
        &items,
        &FailingProbe,
        &executable,
        None,
        None,
    );
    assert!(
        matches!(&results[0].1, RunOutcome::Failed(CaskFailure::AppCheckFailed { app, .. }) if app == "a")
    );
    assert!(calls(&runner).is_empty());
}

#[test]
fn a_blocked_cask_is_refused_before_brew_is_asked_anything() {
    let dir = tempfile::tempdir().unwrap();
    let mut needy = item(dir.path(), "a");
    needy.blocked = Some(CaskFailure::NeedsAdministrator);
    let runner = runner_for(&["a"]);
    let results = run_updates(
        &brew(&runner),
        &[needy],
        &MockChecker::none(),
        &executable,
        None,
        None,
    );
    assert_eq!(
        results[0].1,
        RunOutcome::Failed(CaskFailure::NeedsAdministrator)
    );
    assert!(calls(&runner).is_empty());
}

#[test]
fn a_failed_cask_does_not_stop_the_next() {
    let dir = tempfile::tempdir().unwrap();
    let items = [item(dir.path(), "a"), item(dir.path(), "b")];
    let runner = runner_for(&["a", "b"]).with_exit(BREW_PATH, &["upgrade", "--cask", "a"], 1, "");
    let results = run_updates(
        &brew(&runner),
        &items,
        &MockChecker::none(),
        &executable,
        None,
        None,
    );
    assert!(matches!(
        results[0].1,
        RunOutcome::Failed(CaskFailure::BrewFailed { .. })
    ));
    assert_eq!(results[1].1, RunOutcome::Done);
}

#[test]
fn cancelling_stops_the_run_and_leaves_the_rest_unattempted() {
    let dir = tempfile::tempdir().unwrap();
    let items = [item(dir.path(), "a"), item(dir.path(), "b")];
    let runner = runner_for(&["a", "b"]);
    let cancel = AtomicBool::new(false);
    cancel.store(true, Ordering::Release);
    let results = run_updates(
        &brew(&runner),
        &items,
        &MockChecker::none(),
        &executable,
        Some(&cancel),
        None,
    );
    assert_eq!(results[0].1, RunOutcome::Cancelled);
    assert_eq!(results[1].1, RunOutcome::NotAttempted);
    assert!(!calls(&runner).iter().any(|call| call.contains("upgrade")));
}

// ---- before a run is accepted

#[test]
fn the_apps_are_checked_before_anything_is_accepted_and_nothing_is_spawned() {
    let dir = tempfile::tempdir().unwrap();
    let items = [item(dir.path(), "a"), item(dir.path(), "b")];
    let closed = check_apps_closed(&items, &MockChecker::none(), &executable);
    assert_eq!(closed, Ok(()));
    let open = check_apps_closed(&items, &MockChecker::with_running(["bExec"]), &executable);
    assert_eq!(open, Err(CaskFailure::AppRunning { app: "b".into() }));
    let unknown = check_apps_closed(&items, &FailingProbe, &executable);
    assert!(matches!(unknown, Err(CaskFailure::AppCheckFailed { .. })));
    assert_eq!(check_apps_closed(&[], &FailingProbe, &executable), Ok(()));
}
