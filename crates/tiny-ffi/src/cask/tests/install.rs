use super::*;

// ---- install preview

#[test]
fn an_install_preview_names_version_host_command_and_expiry() {
    let world = installable();
    let preview = preview_install(&world);
    assert_eq!(preview.action, FfiCaskAction::Install);
    assert_eq!(preview.expires_in_seconds, 15 * 60);
    assert!(preview.expires_at > 0 && preview.warnings.is_empty());
    let [item] = &preview.items[..] else {
        panic!("one item per install");
    };
    assert_eq!(item.token, "ghostty");
    assert_eq!(item.name, "Ghostty");
    assert_eq!(
        (item.from_version.as_deref(), item.version.as_str()),
        (None, "1.3.1")
    );
    assert_eq!(item.download_host.as_deref(), Some("downloads.example.com"));
    assert_eq!(item.command, "brew install --cask ghostty");
    assert_eq!(item.refusal, None);
    assert_eq!(item.run_state, FfiAppRunState::NotRunning);
    assert_eq!(world.brew.calls(), ["info --cask --json=v2 ghostty"]);
}

#[test]
fn what_cannot_be_installed_is_refused_before_a_preview_exists() {
    let world = installable();
    let kind = |token: &str| {
        failure_kind(
            world
                .session
                .install_preview(token, &world.seams())
                .unwrap_err(),
        )
    };
    assert_eq!(kind("wireshark"), FfiCaskFailureKind::NotInCatalogue);
    assert_eq!(kind("--force"), FfiCaskFailureKind::NotInCatalogue);
    assert_eq!(world.brew.spawned("info"), 0, "nothing was asked of brew");

    world.install_app("My Terminal", GHOSTTY_ID);
    assert_eq!(kind("ghostty"), FfiCaskFailureKind::AppExists);
    assert_eq!(world.brew.spawned("info"), 0);
}

#[test]
fn a_cask_homebrew_lists_or_an_unreadable_inventory_stops_the_preview() {
    let world = World::new();
    world.info(
        "ghostty",
        Spec {
            installed: Some("1.0"),
            ..Spec::default()
        },
    );
    let listed = world
        .session
        .install_preview("ghostty", &world.seams())
        .unwrap_err();
    assert_eq!(failure_kind(listed), FfiCaskFailureKind::AlreadyInstalled);

    let world = installable();
    world.catalog.unreadable.store(true, Ordering::SeqCst);
    let unreadable = world
        .session
        .install_preview("ghostty", &world.seams())
        .unwrap_err();
    assert!(matches!(unreadable, FfiCaskError::Operation { .. }));
    assert!(
        world.brew.calls().is_empty(),
        "fail closed before asking brew"
    );
}

#[test]
fn a_cask_that_needs_a_password_or_a_manual_installer_gets_a_preview_with_its_reason() {
    for (artifacts, kind) in [
        (
            r#"{"pkg":["Ghostty.pkg"]}"#,
            FfiCaskFailureKind::NeedsAdministrator,
        ),
        (
            r#"{"installer":[{"manual":"Setup.app"}]}"#,
            FfiCaskFailureKind::ManualInstaller,
        ),
    ] {
        let world = World::new();
        world.info(
            "ghostty",
            Spec {
                extra_artifacts: artifacts,
                ..Spec::default()
            },
        );
        let item = &preview_install(&world).items[0];
        let refusal = item.refusal.as_ref().expect("a reason");
        assert_eq!(refusal.kind, kind);
        assert!(!refusal.problem.is_empty() && !refusal.recovery.is_empty());
    }
}

#[test]
fn a_busy_session_or_a_missing_brew_stops_the_preview() {
    let world = installable();
    let held = world.session.begin().unwrap();
    assert!(matches!(
        world.session.install_preview("ghostty", &world.seams()),
        Err(FfiCaskError::Busy)
    ));
    drop(held);
    *world.brew.path.lock().unwrap() = None;
    let error = world
        .session
        .install_preview("ghostty", &world.seams())
        .unwrap_err();
    assert_eq!(failure_kind(error), FfiCaskFailureKind::HomebrewMissing);
}

#[test]
fn a_brew_env_setting_that_undoes_ours_is_carried_in_the_preview() {
    let world = installable();
    let env = world.root.join("prefix/etc/homebrew/brew.env");
    std::fs::create_dir_all(env.parent().unwrap()).unwrap();
    std::fs::write(&env, "HOMEBREW_NO_SUDO=0\n").unwrap();
    let preview = preview_install(&world);
    assert_eq!(preview.warnings.len(), 1);
    assert_eq!(preview.warnings[0].variable, "HOMEBREW_NO_SUDO");
}

// ---- install execute

pub(super) fn install_ok(world: &World) {
    world.brew.reply(
        INSTALL,
        0,
        "==> Installing Cask ghostty\n==> Moving App 'Ghostty.app'\n",
        "==> Fetching downloads for: ghostty\n",
    );
}

#[test]
fn an_install_runs_the_exact_command_once_and_reports_each_stage_with_the_token() {
    let world = installable();
    install_ok(&world);
    let preview = preview_install(&world);
    let report = world
        .exec(&preview.preview_id, &["ghostty"], &token())
        .unwrap();
    assert_eq!(report.action, FfiCaskAction::Install);
    assert_eq!(
        (report.done_count, report.failed_count, report.cancelled),
        (1, 0, false)
    );
    let [result] = &report.results[..] else {
        panic!("one result");
    };
    assert_eq!(
        (result.token.as_str(), result.name.as_str()),
        ("ghostty", "Ghostty")
    );
    assert_eq!(result.outcome, FfiCaskOutcome::Done);
    assert!(result.app_path.as_deref().unwrap().ends_with("Ghostty.app"));
    assert_eq!(world.brew.spawned("install"), 1);
    assert_eq!(world.brew.calls().last().unwrap(), INSTALL);
    let step = |stage| ("ghostty".to_string(), stage, 0, 1);
    assert_eq!(
        world.steps(),
        [
            step(FfiCaskStage::Working),
            step(FfiCaskStage::Downloading),
            step(FfiCaskStage::Installing)
        ]
    );
    assert!(!world.session.is_busy(), "the gate is released");
}

#[test]
fn a_preview_runs_once_and_a_replay_says_so() {
    let world = installable();
    install_ok(&world);
    let preview = preview_install(&world);
    world
        .exec(&preview.preview_id, &["ghostty"], &token())
        .unwrap();
    let again = world
        .exec(&preview.preview_id, &["ghostty"], &token())
        .unwrap_err();
    assert!(
        matches!(&again, FfiCaskError::PreviewInvalid { detail } if detail == "preview already used"),
        "{again:?}"
    );
    assert_eq!(world.brew.spawned("install"), 1);
}

#[test]
fn an_unknown_replaced_or_expired_preview_never_runs() {
    let world = installable();
    install_ok(&world);
    let first = preview_install(&world);
    let second = preview_install(&world);
    for id in [first.preview_id.as_str(), "k999", ""] {
        let error = world.exec(id, &["ghostty"], &token()).unwrap_err();
        assert!(
            matches!(error, FfiCaskError::PreviewInvalid { .. }),
            "{id:?}"
        );
    }
    world.session.cask_state().ttl = Some(Duration::ZERO);
    let expired = preview_install(&world);
    let error = world
        .exec(&expired.preview_id, &["ghostty"], &token())
        .unwrap_err();
    assert!(
        matches!(&error, FfiCaskError::PreviewInvalid { detail } if detail == "preview expired"),
        "{error:?}"
    );
    assert_ne!(first.preview_id, second.preview_id);
    assert_eq!(world.brew.spawned("install"), 0);
}

#[test]
fn a_bad_selection_is_refused_and_leaves_the_preview_usable() {
    let world = installable();
    install_ok(&world);
    let preview = preview_install(&world);
    for tokens in [
        &[][..],
        &["rectangle"],
        &["ghostty", "ghostty"],
        &["--force"],
    ] {
        let error = world
            .exec(&preview.preview_id, tokens, &token())
            .unwrap_err();
        assert!(
            matches!(error, FfiCaskError::InvalidInput { .. }),
            "{tokens:?}"
        );
    }
    assert_eq!(world.brew.spawned("install"), 0);
    let report = world
        .exec(&preview.preview_id, &["ghostty"], &token())
        .unwrap();
    assert_eq!(report.done_count, 1, "still usable after the refusals");
}

#[test]
fn a_busy_session_a_cancelled_token_or_a_vanished_brew_refuses_without_spending_the_preview() {
    let world = installable();
    install_ok(&world);
    let preview = preview_install(&world);

    let held = world.session.begin().unwrap();
    let busy = world
        .exec(&preview.preview_id, &["ghostty"], &token())
        .unwrap_err();
    assert!(matches!(busy, FfiCaskError::Busy));
    drop(held);

    let cancelled = token();
    cancelled.cancel();
    let error = world
        .exec(&preview.preview_id, &["ghostty"], &cancelled)
        .unwrap_err();
    assert!(matches!(error, FfiCaskError::Cancelled));

    let path = world.brew.path.lock().unwrap().take();
    let gone = world
        .exec(&preview.preview_id, &["ghostty"], &token())
        .unwrap_err();
    assert_eq!(failure_kind(gone), FfiCaskFailureKind::HomebrewMissing);
    *world.brew.path.lock().unwrap() = path;

    assert_eq!(world.brew.spawned("install"), 0);
    let report = world
        .exec(&preview.preview_id, &["ghostty"], &token())
        .unwrap();
    assert_eq!(report.done_count, 1);
}

#[test]
fn a_cask_that_needs_a_password_is_refused_at_execute_and_nothing_runs() {
    let world = World::new();
    world.info(
        "ghostty",
        Spec {
            extra_artifacts: r#"{"pkg":["Ghostty.pkg"]}"#,
            ..Spec::default()
        },
    );
    let preview = preview_install(&world);
    let error = world
        .exec(&preview.preview_id, &["ghostty"], &token())
        .unwrap_err();
    assert_eq!(failure_kind(error), FfiCaskFailureKind::NeedsAdministrator);
    assert_eq!(world.brew.spawned("install"), 0);
}

#[test]
fn an_app_that_appeared_where_the_cask_would_go_is_refused_at_execute() {
    let world = installable();
    install_ok(&world);
    let preview = preview_install(&world);
    std::fs::create_dir(world.app("Ghostty")).unwrap();
    let error = world
        .exec(&preview.preview_id, &["ghostty"], &token())
        .unwrap_err();
    assert_eq!(failure_kind(error), FfiCaskFailureKind::AppExists);
    assert_eq!(world.brew.spawned("install"), 0);
}

#[test]
fn a_brew_failure_is_in_the_report_with_its_problem_and_recovery() {
    let world = installable();
    world.brew.reply(
        INSTALL,
        1,
        "",
        "Error: Download failed on Cask 'ghostty' with message: curl: (6) Could not resolve host: x",
    );
    let preview = preview_install(&world);
    let report = world
        .exec(&preview.preview_id, &["ghostty"], &token())
        .unwrap();
    assert_eq!((report.done_count, report.failed_count), (0, 1));
    let FfiCaskOutcome::Failed { failure } = &report.results[0].outcome else {
        panic!("failed outcome");
    };
    assert_eq!(failure.kind, FfiCaskFailureKind::Network);
    assert!(failure.problem.ends_with('.') && failure.recovery.ends_with('.'));
    assert!(failure
        .detail
        .as_deref()
        .unwrap()
        .contains("Could not resolve host"));
}

#[test]
fn a_cancel_raised_as_the_run_begins_stops_it_before_brew_and_is_reported() {
    let world = installable();
    install_ok(&world);
    let preview = preview_install(&world);
    let cancel = token();
    *world.listener.cancel_on_first_step.lock().unwrap() = Some(cancel.clone());
    let report = world
        .exec(&preview.preview_id, &["ghostty"], &cancel)
        .unwrap();
    assert!(report.cancelled);
    assert_eq!(report.results[0].outcome, FfiCaskOutcome::Cancelled);
    assert_eq!(world.brew.spawned("install"), 0);
    let replay = world.exec(&preview.preview_id, &["ghostty"], &token());
    assert!(
        matches!(replay, Err(FfiCaskError::PreviewInvalid { .. })),
        "it was spent"
    );
}

#[test]
fn a_missing_home_is_not_reported_as_a_missing_homebrew() {
    let mut world = installable();
    world.brew.home = None;
    let error = world
        .session
        .install_preview("ghostty", &world.seams())
        .unwrap_err();
    assert!(
        matches!(&error, FfiCaskError::Operation { detail } if detail.contains("home folder")),
        "{error:?}"
    );
    let listing = world.session.outdated(&world.seams()).unwrap_err();
    assert!(matches!(listing, FfiCaskError::Operation { .. }));
    assert!(world.brew.calls().is_empty());
}

#[test]
fn an_app_kept_in_the_users_applications_folder_stops_the_install_too() {
    let world = installable();
    world.keep_in_user_applications("Terminal I Like", GHOSTTY_ID);
    let error = world
        .session
        .install_preview("ghostty", &world.seams())
        .unwrap_err();
    let FfiCaskError::Failed { failure } = error else {
        panic!("a typed failure");
    };
    assert_eq!(failure.kind, FfiCaskFailureKind::AppExists);
    assert!(failure
        .subject
        .as_deref()
        .unwrap()
        .ends_with("home/Applications/Terminal I Like.app"));
    assert!(world.brew.calls().is_empty());
}
