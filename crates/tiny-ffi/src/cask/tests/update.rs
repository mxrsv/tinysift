use super::*;

// ---- update preview

#[test]
fn an_update_preview_needs_a_current_list_and_only_tokens_from_it() {
    let world = World::new();
    let none = world
        .session
        .update_preview(&["ghostty".into()], &world.seams());
    assert!(matches!(none, Err(FfiCaskError::PreviewInvalid { .. })));

    let world = updatable();
    world.brew.clear_calls();
    let rejected = |tokens: &[&str]| {
        let tokens: Vec<String> = tokens.iter().map(|t| t.to_string()).collect();
        world
            .session
            .update_preview(&tokens, &world.seams())
            .unwrap_err()
    };
    // A bad request is not a cask that is up to date: the text must not lie.
    for tokens in [
        &["rectangle"][..],
        &["ghostty", "ghostty"],
        &["--greedy"],
        &["user/tap/ghostty"],
        &[],
    ] {
        assert!(
            matches!(rejected(tokens), FfiCaskError::InvalidInput { .. }),
            "{tokens:?}"
        );
    }
    assert_eq!(world.brew.spawned("info"), 0, "nothing was asked of brew");
}

#[test]
fn a_listed_cask_that_is_no_longer_outdated_or_is_past_the_cask_is_not_outdated() {
    let world = updatable();
    // It left `brew outdated` since the list.
    world.info(
        "ghostty",
        Spec {
            outdated: false,
            ..outdated_spec("Ghostty")
        },
    );
    let gone = world
        .session
        .update_preview(&["ghostty".into()], &world.seams())
        .unwrap_err();
    assert_eq!(failure_kind(gone), FfiCaskFailureKind::NotOutdated);
    // The app is past the cask, which `brew outdated` still lists.
    world.info(
        "ghostty",
        Spec {
            bundle_version: Some("3.0"),
            ..outdated_spec("Ghostty")
        },
    );
    let past = world
        .session
        .update_preview(&["ghostty".into()], &world.seams())
        .unwrap_err();
    assert_eq!(failure_kind(past), FfiCaskFailureKind::NotOutdated);
}

#[test]
fn an_update_preview_lists_each_cask_with_versions_command_and_whether_it_is_open() {
    let world = updatable();
    world.open(PROCESS);
    world.info_all(&[
        ("iterm2", outdated_spec("iTerm")),
        ("ghostty", outdated_spec("Ghostty")),
    ]);
    let preview = preview_update(&world, &["iterm2", "ghostty"]);
    assert_eq!(preview.action, FfiCaskAction::Update);
    let seen: Vec<_> = preview
        .items
        .iter()
        .map(|i| {
            (
                i.token.as_str(),
                i.from_version.as_deref(),
                i.version.as_str(),
                i.command.as_str(),
            )
        })
        .collect();
    assert_eq!(
        seen,
        [
            (
                "iterm2",
                Some("1.0"),
                "2.0",
                "brew upgrade --cask --no-quit iterm2"
            ),
            (
                "ghostty",
                Some("1.0"),
                "2.0",
                "brew upgrade --cask --no-quit ghostty"
            ),
        ]
    );
    // Every app here has the same process name, so both read as open.
    assert!(preview
        .items
        .iter()
        .all(|i| i.run_state == FfiAppRunState::Running));
}

#[test]
fn an_update_that_needs_a_password_is_previewed_with_its_reason() {
    let world = World::new();
    world.outdated_list(&[("ghostty", "1.0", "2.0")]);
    world.info(
        "ghostty",
        Spec {
            version: "2.0",
            installed: Some("1.0"),
            outdated: true,
            extra_artifacts: r#"{"uninstall":[{"pkgutil":"com.example.pkg"}]}"#,
            ..Spec::default()
        },
    );
    let listed = world.session.outdated(&world.seams()).unwrap();
    assert_eq!(
        listed[0].refusal.as_ref().unwrap().kind,
        FfiCaskFailureKind::NeedsAdministrator
    );
    let preview = preview_update(&world, &["ghostty"]);
    assert_eq!(
        preview.items[0].refusal.as_ref().unwrap().kind,
        FfiCaskFailureKind::NeedsAdministrator
    );
}

// ---- update execute

#[test]
fn updates_run_one_at_a_time_in_the_previews_order_with_progress_naming_each_cask() {
    let world = updatable();
    upgrade_replies(&world, &["ghostty", "iterm2"]);
    let preview = preview_update(&world, &["ghostty", "iterm2"]);
    world.brew.clear_calls();
    let report = world
        .exec(&preview.preview_id, &["iterm2", "ghostty"], &token())
        .unwrap();
    assert_eq!(report.action, FfiCaskAction::Update);
    assert_eq!((report.done_count, report.failed_count), (2, 0));
    let ran: Vec<_> = report.results.iter().map(|r| r.token.as_str()).collect();
    assert_eq!(
        ran,
        ["ghostty", "iterm2"],
        "the preview's order, not the selection's"
    );
    assert_eq!(
        world.brew.calls(),
        [
            "outdated --cask --json=v2",
            "info --cask --json=v2 ghostty",
            "upgrade --cask --no-quit ghostty",
            "outdated --cask --json=v2",
            "info --cask --json=v2 iterm2",
            "upgrade --cask --no-quit iterm2"
        ]
    );
    let seen = world.steps();
    let tokens: Vec<_> = seen.iter().map(|(t, ..)| t.as_str()).collect();
    assert_eq!(tokens, ["ghostty", "ghostty", "iterm2", "iterm2"]);
    assert!(seen.iter().all(|(_, _, _, total)| *total == 2));
    assert_eq!(seen[2].2, 1, "the second cask is at position 1");
}

#[test]
fn only_the_selected_casks_run() {
    let world = updatable();
    upgrade_replies(&world, &["ghostty", "iterm2"]);
    let preview = preview_update(&world, &["ghostty", "iterm2"]);
    let report = world
        .exec(&preview.preview_id, &["iterm2"], &token())
        .unwrap();
    assert_eq!(report.results.len(), 1);
    assert_eq!(report.results[0].token, "iterm2");
    assert_eq!(world.brew.spawned("upgrade"), 1);
}

#[test]
fn an_open_app_refuses_the_run_before_it_is_spent_and_a_retry_after_quitting_works() {
    let world = updatable();
    upgrade_replies(&world, &["ghostty", "iterm2"]);
    let preview = preview_update(&world, &["ghostty", "iterm2"]);
    world.open(PROCESS);
    let error = world
        .exec(&preview.preview_id, &["ghostty"], &token())
        .unwrap_err();
    let FfiCaskError::Failed { failure } = error else {
        panic!("a typed refusal");
    };
    assert_eq!(failure.kind, FfiCaskFailureKind::AppRunning);
    assert_eq!(
        failure.subject.as_deref(),
        Some("Ghostty"),
        "it names the app"
    );
    assert_eq!(world.brew.spawned("upgrade"), 0);

    world.close_everything();
    let report = world
        .exec(&preview.preview_id, &["ghostty", "iterm2"], &token())
        .unwrap();
    assert_eq!(
        report.done_count, 2,
        "the same preview, because nothing ran"
    );
}

#[test]
fn an_app_whose_state_cannot_be_told_is_not_updated() {
    let world = updatable();
    upgrade_replies(&world, &["ghostty"]);
    world.info("ghostty", outdated_spec("Ghostty"));
    let preview = preview_update(&world, &["ghostty"]);
    world.probe.fails.store(true, Ordering::SeqCst);
    let error = world
        .exec(&preview.preview_id, &["ghostty"], &token())
        .unwrap_err();
    assert_eq!(failure_kind(error), FfiCaskFailureKind::AppCheckFailed);

    world.probe.fails.store(false, Ordering::SeqCst);
    *world.catalog.executable.lock().unwrap() = None;
    let nameless = world
        .exec(&preview.preview_id, &["ghostty"], &token())
        .unwrap_err();
    assert_eq!(failure_kind(nameless), FfiCaskFailureKind::AppCheckFailed);
    assert_eq!(world.brew.spawned("upgrade"), 0);
}

#[test]
fn a_cask_that_updated_itself_after_the_preview_is_reported_not_upgraded() {
    let world = updatable();
    upgrade_replies(&world, &["ghostty", "iterm2"]);
    let preview = preview_update(&world, &["ghostty", "iterm2"]);
    // iTerm updated itself meanwhile: the default list no longer names it.
    world.outdated_list(&[("ghostty", "1.0", "2.0")]);
    let report = world
        .exec(&preview.preview_id, &["ghostty", "iterm2"], &token())
        .unwrap();
    assert_eq!((report.done_count, report.failed_count), (1, 1));
    let FfiCaskOutcome::Failed { failure } = &report.results[1].outcome else {
        panic!("iterm2 fails");
    };
    assert_eq!(failure.kind, FfiCaskFailureKind::NotOutdated);
    assert_eq!(
        world
            .brew
            .calls()
            .iter()
            .filter(|c| *c == "upgrade --cask --no-quit iterm2")
            .count(),
        0
    );
}

#[test]
fn a_cancel_leaves_the_rest_of_a_batch_not_attempted() {
    let world = updatable();
    upgrade_replies(&world, &["ghostty", "iterm2"]);
    let preview = preview_update(&world, &["ghostty", "iterm2"]);
    let cancel = token();
    *world.listener.cancel_on_first_step.lock().unwrap() = Some(cancel.clone());
    let report = world
        .exec(&preview.preview_id, &["ghostty", "iterm2"], &cancel)
        .unwrap();
    assert!(report.cancelled);
    assert_eq!(report.results[0].outcome, FfiCaskOutcome::Cancelled);
    assert_eq!(report.results[1].outcome, FfiCaskOutcome::NotAttempted);
    assert_eq!((report.done_count, report.failed_count), (0, 0));
    assert_eq!(world.brew.spawned("upgrade"), 0);
}
