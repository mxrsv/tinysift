use super::*;

fn item(dir: &Path, token: &str) -> CaskPlanItem {
    plan_item(CaskAction::Update, token, installed_app(dir, token))
}

fn outdated_call() -> String {
    format!("{BREW_PATH} outdated --cask --json=v2")
}

fn upgrade(token: &str) -> String {
    format!("{BREW_PATH} upgrade --cask --no-quit {token}")
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
            runner
                .with_exit(
                    BREW_PATH,
                    &["info", "--cask", "--json=v2", token],
                    0,
                    &fresh_info(token, "1.0"),
                )
                .with_exit(
                    BREW_PATH,
                    &["upgrade", "--cask", "--no-quit", token],
                    0,
                    "==> Upgrading x\n==> Moving App 'x.app'\n",
                )
        },
    )
}

fn info_call(token: &str) -> String {
    format!("{BREW_PATH} info --cask --json=v2 {token}")
}

/// `brew info` as `run_one` reads it right before an upgrade: installed 1.0,
/// the app on disk at `bundle`, the cask at 2.0.
fn fresh_info(token: &str, bundle: &str) -> String {
    let mut info = simple_cask(token, "2.0", &format!("{token}.app"));
    info["installed"] = serde_json::json!("1.0");
    info["bundle_short_version"] = serde_json::json!(bundle);
    info["outdated"] = serde_json::json!(true);
    info_json(&[info])
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
        [
            outdated_call(),
            info_call("a"),
            upgrade("a"),
            outdated_call(),
            info_call("b"),
            upgrade("b")
        ]
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
    // `brew upgrade --cask --no-quit b` would act greedily and reinstall it, so the
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
    let runner = runner_for(&["a", "b"]).with_exit(
        BREW_PATH,
        &["upgrade", "--cask", "--no-quit", "a"],
        1,
        "",
    );
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

/// A Homebrew whose `verb` call also opens the app, as a user can while a
/// read takes its half second.
struct OpeningRunner<'a> {
    runner: MockRunner,
    running: &'a AtomicBool,
    verb: &'static str,
}

impl crate::runner::CommandRunner for OpeningRunner<'_> {
    fn which(&self, bin: &str) -> bool {
        self.runner.which(bin)
    }
    fn run(&self, bin: &str, args: &[&str]) -> crate::runner::CommandOutput {
        self.runner.run(bin, args)
    }
    fn output(
        &self,
        bin: &str,
        args: &[&str],
        timeout: std::time::Duration,
    ) -> Result<crate::runner::CommandOutcome, crate::runner::CommandError> {
        let result = self.runner.output(bin, args, timeout);
        if args.first() == Some(&self.verb) {
            self.running.store(true, Ordering::Release);
        }
        result
    }
}

struct OpenProbe(AtomicBool);

impl AppProbe for OpenProbe {
    fn probe(&self, _: &str) -> Result<bool, String> {
        Ok(self.0.load(Ordering::Acquire))
    }
}

#[test]
fn an_app_opened_while_brew_is_read_is_not_upgraded_whichever_read_it_was() {
    for (verb, expected_calls) in [
        ("outdated", vec![outdated_call(), info_call("a")]),
        ("info", vec![outdated_call(), info_call("a")]),
    ] {
        let dir = tempfile::tempdir().unwrap();
        let items = [item(dir.path(), "a")];
        let probe = OpenProbe(AtomicBool::new(false));
        let runner = OpeningRunner {
            runner: runner_for(&["a"]),
            running: &probe.0,
            verb,
        };
        let result = run_updates(
            &Brew::new(Path::new(BREW_PATH), &runner),
            &items,
            &probe,
            &executable,
            None,
            None,
        );
        assert_eq!(
            result[0].1,
            RunOutcome::Failed(CaskFailure::AppRunning { app: "a".into() }),
            "opened during the {verb} read"
        );
        assert_eq!(
            calls(&runner.runner),
            expected_calls,
            "a read must not turn into an upgrade or a quit"
        );
    }
}

#[test]
fn an_app_already_past_the_cask_is_not_downgraded() {
    // The reviewer's fixture. `brew outdated` still lists `a` (its record,
    // 1.0, differs from the cask's 2.0) but the app updated itself to 3.0.
    let dir = tempfile::tempdir().unwrap();
    let items = [item(dir.path(), "a"), item(dir.path(), "b")];
    let runner = runner_for(&["a", "b"]).with_exit(
        BREW_PATH,
        &["info", "--cask", "--json=v2", "a"],
        0,
        &fresh_info("a", "3.0"),
    );
    let results = run_updates(
        &brew(&runner),
        &items,
        &MockChecker::none(),
        &executable,
        None,
        None,
    );
    assert_eq!(
        results[0].1,
        RunOutcome::Failed(CaskFailure::NotOutdated { token: "a".into() })
    );
    assert_eq!(results[1].1, RunOutcome::Done, "the next cask goes on");
    assert!(!calls(&runner).contains(&upgrade("a")));
}

#[test]
fn a_failing_fresh_read_of_the_versions_stops_that_cask_before_the_upgrade() {
    let dir = tempfile::tempdir().unwrap();
    let items = [item(dir.path(), "a")];
    let runner =
        runner_for(&["a"]).with_exit(BREW_PATH, &["info", "--cask", "--json=v2", "a"], 1, "");
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
    assert!(!calls(&runner).contains(&upgrade("a")));
}
