use super::*;

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
