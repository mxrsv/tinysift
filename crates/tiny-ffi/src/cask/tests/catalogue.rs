use super::*;

// ---- status and catalogue

#[test]
fn the_status_names_brew_and_the_settings_that_override_ours() {
    let world = World::new();
    let env = world.root.join("home/.homebrew/brew.env");
    std::fs::create_dir_all(env.parent().unwrap()).unwrap();
    std::fs::write(&env, "HOMEBREW_UPGRADE_GREEDY=1\nHOMEBREW_NO_ANALYTICS=1\n").unwrap();
    let FfiHomebrewStatus::Present { path, warnings } = homebrew_status_of(&world.brew) else {
        panic!("brew is present");
    };
    assert!(path.ends_with("prefix/bin/brew"));
    assert_eq!(
        warnings,
        [FfiBrewEnvWarning {
            file: env.to_string_lossy().into_owned(),
            variable: "HOMEBREW_UPGRADE_GREEDY".into()
        }]
    );
    *world.brew.path.lock().unwrap() = None;
    assert_eq!(homebrew_status_of(&world.brew), FfiHomebrewStatus::Missing);
}

#[test]
fn the_catalogue_marks_apps_by_identity_and_by_homebrews_record() {
    let world = World::new();
    world.install_app("My Terminal", GHOSTTY_ID);
    world.brew.reply(
        "list --cask --versions --json",
        0,
        r#"{"formulae":[],"casks":[{"token":"iterm2","versions":["3.7.6"]},{"token":"ghostty","versions":["1.0"]}]}"#,
        "",
    );
    let apps = world.session.catalogue(&world.seams()).unwrap();
    let row = |token: &str| apps.iter().find(|a| a.token == token).unwrap();
    assert_eq!(apps.len(), 7);
    assert!(row("ghostty").on_this_mac && row("ghostty").tracked_by_homebrew);
    assert!(!row("iterm2").on_this_mac && row("iterm2").tracked_by_homebrew);
    assert!(!row("rectangle").on_this_mac && !row("rectangle").tracked_by_homebrew);
    assert_eq!(row("chatgpt").bundle_id, "com.openai.codex");
}

#[test]
fn the_catalogue_still_lists_without_homebrew_or_when_it_cannot_say() {
    let world = World::new();
    *world.brew.path.lock().unwrap() = None;
    assert_eq!(world.session.catalogue(&world.seams()).unwrap().len(), 7);
    assert!(world.brew.calls().is_empty());

    let world = World::new();
    world
        .brew
        .reply("list --cask --versions --json", 1, "", "Error: broken");
    let apps = world.session.catalogue(&world.seams()).unwrap();
    assert!(apps.iter().all(|a| !a.tracked_by_homebrew));
}

#[test]
fn an_unreadable_inventory_fails_the_catalogue_and_a_busy_session_refuses_it() {
    let world = World::new();
    world.catalog.unreadable.store(true, Ordering::SeqCst);
    assert!(matches!(
        world.session.catalogue(&world.seams()),
        Err(FfiCaskError::Operation { .. })
    ));
    let held = world.session.begin().unwrap();
    assert!(matches!(
        world.session.catalogue(&world.seams()),
        Err(FfiCaskError::Busy)
    ));
    drop(held);
}

// ---- outdated

#[test]
fn the_update_list_says_which_apps_are_open_and_replaces_any_preview() {
    let world = updatable();
    world.open(PROCESS);
    let listed = world.session.outdated(&world.seams()).unwrap();
    let tokens: Vec<_> = listed.iter().map(|c| c.token.as_str()).collect();
    assert_eq!(tokens, ["ghostty", "iterm2"]);
    assert_eq!(listed[0].from_version, "1.0");
    assert_eq!(listed[0].version, "2.0");
    assert_eq!(listed[0].refusal, None);
    assert_eq!(listed[0].run_state, FfiAppRunState::Running);
    assert!(listed[0]
        .app_path
        .as_deref()
        .unwrap()
        .ends_with("Ghostty.app"));
    assert_eq!(
        world
            .brew
            .calls()
            .iter()
            .filter(|c| c.contains("--greedy"))
            .count(),
        0
    );
}

#[test]
fn a_new_update_list_drops_the_preview_built_from_the_old_one() {
    let world = updatable();
    world.info("ghostty", outdated_spec("Ghostty"));
    let preview = preview_update(&world, &["ghostty"]);
    world.session.outdated(&world.seams()).unwrap();
    let error = world
        .exec(&preview.preview_id, &["ghostty"], &token())
        .unwrap_err();
    assert!(matches!(error, FfiCaskError::PreviewInvalid { .. }));
}

#[test]
fn listing_updates_needs_brew_and_reports_its_errors_typed() {
    let world = World::new();
    *world.brew.path.lock().unwrap() = None;
    let missing = world.session.outdated(&world.seams()).unwrap_err();
    assert_eq!(failure_kind(missing), FfiCaskFailureKind::HomebrewMissing);

    let world = World::new();
    world.brew.reply(
        "outdated --cask --json=v2",
        1,
        "",
        "curl: (6) Could not resolve host: formulae.brew.sh",
    );
    let offline = world.session.outdated(&world.seams()).unwrap_err();
    assert_eq!(failure_kind(offline), FfiCaskFailureKind::Network);

    let held = world.session.begin().unwrap();
    assert!(matches!(
        world.session.outdated(&world.seams()),
        Err(FfiCaskError::Busy)
    ));
    drop(held);
}
