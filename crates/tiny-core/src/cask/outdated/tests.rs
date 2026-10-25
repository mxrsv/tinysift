use std::path::Path;

use serde_json::json;

use super::*;
use crate::cask::test_support::*;
use crate::runner::test_support::MockRunner;
use crate::runner::CommandError;

const OUTDATED_ARGS: [&str; 3] = ["outdated", "--cask", "--json=v2"];

fn list(runner: &MockRunner) -> Result<Vec<OutdatedCask>, CaskFailure> {
    list_outdated(&Brew::new(Path::new(BREW_PATH), runner))
}

/// What the Mac's Homebrew would say: an app updated by itself, a command-line
/// tool, a pinned app and a cask from a third-party tap, besides one plain app.
fn mac() -> MockRunner {
    let mut tinycast = simple_cask("tinycast", "0.12.1", "Tinycast.app");
    tinycast["installed"] = json!("0.10.23");
    tinycast["bundle_short_version"] = json!("0.11.0");
    let mut iterm2 = simple_cask("iterm2", "3.7.6", "iTerm.app");
    iterm2["installed"] = json!("3.5.14");
    let tool = cask(
        "codex",
        "0.163.0",
        json!([{"binary": ["codex"], "target": "/opt/homebrew/bin/codex"}]),
    );
    MockRunner::new()
        .with_exit(
            BREW_PATH,
            &OUTDATED_ARGS,
            0,
            &outdated_json(&[
                ("codex", "0.116.0", "0.163.0", false),
                ("iterm2", "3.5.14", "3.7.6", false),
                ("pinned-app", "1.0", "2.0", true),
                ("someone/tap/thing", "1.0", "2.0", false),
                ("tinycast", "0.10.23", "0.12.1", false),
            ]),
        )
        .with_exit(
            BREW_PATH,
            &["info", "--cask", "--json=v2", "codex", "iterm2", "tinycast"],
            0,
            &info_json(&[tool, iterm2, tinycast]),
        )
}

#[test]
fn only_tracked_apps_with_a_newer_version_are_listed() {
    let runner = mac();
    let listed = list(&runner).unwrap();
    let names: Vec<_> = listed
        .iter()
        .map(|c| (c.token.as_str(), c.name.as_str()))
        .collect();
    assert_eq!(names, [("iterm2", "iterm2"), ("tinycast", "tinycast")]);
    assert_eq!(listed[0].version, "3.7.6");
    assert_eq!(listed[0].apps, [Path::new("/Applications/iTerm.app")]);
    assert_eq!(
        listed[0].download_host.as_deref(),
        Some("downloads.example.com")
    );
    assert_eq!(listed[0].blocked, None);
}

#[test]
fn brew_is_asked_once_each_without_greedy_and_never_for_a_pinned_or_tap_token() {
    let runner = mac();
    list(&runner).unwrap();
    assert_eq!(
        *runner.output_calls.lock().unwrap(),
        [
            format!("{BREW_PATH} outdated --cask --json=v2"),
            format!("{BREW_PATH} info --cask --json=v2 codex iterm2 tinycast"),
        ]
    );
}

#[test]
fn from_is_the_app_on_disk_when_brew_read_it_else_what_brew_recorded() {
    let listed = list(&mac()).unwrap();
    assert_eq!(
        listed[0].from_version, "3.5.14",
        "recorded: no bundle version"
    );
    assert_eq!(
        listed[1].from_version, "0.11.0",
        "the app updated itself part way"
    );
}

#[test]
fn nothing_outdated_asks_brew_for_nothing_more() {
    let runner = MockRunner::new().with_exit(BREW_PATH, &OUTDATED_ARGS, 0, &outdated_json(&[]));
    assert_eq!(list(&runner), Ok(Vec::new()));
    assert_eq!(runner.output_calls.lock().unwrap().len(), 1);
}

#[test]
fn failures_are_typed() {
    let missing = MockRunner::new().with_output(
        BREW_PATH,
        &OUTDATED_ARGS,
        Err(CommandError::NotFound(BREW_PATH.into())),
    );
    assert_eq!(list(&missing), Err(CaskFailure::HomebrewMissing));
    let garbled = MockRunner::new().with_exit(BREW_PATH, &OUTDATED_ARGS, 0, "{}");
    assert!(matches!(
        list(&garbled),
        Err(CaskFailure::BrewFailed {
            exit_code: None,
            ..
        })
    ));
    let broken = MockRunner::new().with_exit(BREW_PATH, &OUTDATED_ARGS, 1, "");
    assert!(matches!(
        list(&broken),
        Err(CaskFailure::BrewFailed {
            exit_code: Some(1),
            ..
        })
    ));
}

#[test]
fn an_app_whose_update_needs_a_password_is_listed_blocked() {
    let mut docker = cask(
        "docker-desktop",
        "4.96.0,243774",
        json!([{"uninstall": [{"delete": "/Library/PrivilegedHelperTools/com.docker.vmnetd"}]},
               {"app": ["Docker.app"], "target": "/Applications/Docker.app"}]),
    );
    docker["installed"] = json!("4.90.0");
    let runner = MockRunner::new()
        .with_exit(
            BREW_PATH,
            &OUTDATED_ARGS,
            0,
            &outdated_json(&[("docker-desktop", "4.90.0", "4.96.0,243774", false)]),
        )
        .with_exit(
            BREW_PATH,
            &["info", "--cask", "--json=v2", "docker-desktop"],
            0,
            &info_json(&[docker]),
        );
    let listed = list(&runner).unwrap();
    assert_eq!(listed[0].blocked, Some(CaskFailure::NeedsAdministrator));
    assert_eq!(
        listed[0].version, "4.96.0",
        "the build after the comma is not shown"
    );
}

#[test]
fn a_token_is_still_outdated_only_while_the_default_list_names_it() {
    let brew = |runner| still_outdated(&Brew::new(Path::new(BREW_PATH), runner), "iterm2");
    let listed = MockRunner::new().with_exit(
        BREW_PATH,
        &OUTDATED_ARGS,
        0,
        &outdated_json(&[("iterm2", "3.5.14", "3.7.6", false)]),
    );
    assert_eq!(brew(&listed), Ok(true));
    let pinned = MockRunner::new().with_exit(
        BREW_PATH,
        &OUTDATED_ARGS,
        0,
        &outdated_json(&[("iterm2", "3.5.14", "3.7.6", true)]),
    );
    assert_eq!(brew(&pinned), Ok(false), "a pinned cask is never updated");
    let gone = MockRunner::new().with_exit(BREW_PATH, &OUTDATED_ARGS, 0, &outdated_json(&[]));
    assert_eq!(brew(&gone), Ok(false));
    for runner in [&listed, &pinned, &gone] {
        assert_eq!(
            *runner.output_calls.lock().unwrap(),
            [format!("{BREW_PATH} outdated --cask --json=v2")],
            "never --greedy"
        );
    }
}

#[test]
fn an_app_that_already_is_the_listed_version_is_not_offered_an_update() {
    // Tinycast as the real Mac showed it: Homebrew's record says 0.10.23, the
    // app updated itself to 0.12.1, and the cask is at 0.12.1.
    let mut tinycast = simple_cask("tinycast", "0.12.1", "Tinycast.app");
    tinycast["installed"] = json!("0.10.23");
    tinycast["bundle_short_version"] = json!("0.12.1");
    let runner = MockRunner::new()
        .with_exit(
            BREW_PATH,
            &OUTDATED_ARGS,
            0,
            &outdated_json(&[("tinycast", "0.10.23", "0.12.1", false)]),
        )
        .with_exit(
            BREW_PATH,
            &["info", "--cask", "--json=v2", "tinycast"],
            0,
            &info_json(&[tinycast]),
        );
    assert_eq!(list(&runner), Ok(Vec::new()));
}

#[test]
fn raw_build_updates_remain_listed_with_or_without_a_short_version() {
    for short in [None, Some("2.0")] {
        let mut app = simple_cask("a", "2.0,200", "A.app");
        app["installed"] = json!("2.0,100");
        app["bundle_short_version"] = json!(short);
        app["outdated"] = json!(true);
        let runner = MockRunner::new()
            .with_exit(
                BREW_PATH,
                &OUTDATED_ARGS,
                0,
                &outdated_json(&[("a", "2.0,100", "2.0,200", false)]),
            )
            .with_exit(
                BREW_PATH,
                &["info", "--cask", "--json=v2", "a"],
                0,
                &info_json(&[app]),
            );
        let listed = list(&runner).unwrap();
        assert_eq!(
            listed.len(),
            1,
            "a short version {short:?} cannot prove build 200 installed"
        );
        assert_eq!(
            (&*listed[0].from_version, &*listed[0].version),
            ("2.0", "2.0")
        );
    }
}

#[test]
fn an_app_that_updated_itself_past_the_cask_is_not_offered_a_downgrade() {
    // The reviewer's fixture: the cask is at 2.0, Homebrew's record says 1.0,
    // the app is at 3.0. `brew outdated` lists it because 1.0 differs from 2.0.
    let mut app = simple_cask("a", "2.0", "A.app");
    app["installed"] = json!("1.0");
    app["bundle_short_version"] = json!("3.0");
    app["outdated"] = json!(true);
    let runner = MockRunner::new()
        .with_exit(
            BREW_PATH,
            &OUTDATED_ARGS,
            0,
            &outdated_json(&[("a", "1.0", "2.0", false)]),
        )
        .with_exit(
            BREW_PATH,
            &["info", "--cask", "--json=v2", "a"],
            0,
            &info_json(&[app]),
        );
    assert_eq!(list(&runner), Ok(Vec::new()));
}

#[test]
fn a_version_that_cannot_be_ordered_stays_listed() {
    let mut app = simple_cask("a", "1.9.3b", "A.app");
    app["installed"] = json!("1.9.2");
    app["bundle_short_version"] = json!("1.9.3");
    app["outdated"] = json!(true);
    let runner = MockRunner::new()
        .with_exit(
            BREW_PATH,
            &OUTDATED_ARGS,
            0,
            &outdated_json(&[("a", "1.9.2", "1.9.3b", false)]),
        )
        .with_exit(
            BREW_PATH,
            &["info", "--cask", "--json=v2", "a"],
            0,
            &info_json(&[app]),
        );
    let listed = list(&runner).unwrap();
    assert_eq!(
        (&*listed[0].from_version, &*listed[0].version),
        ("1.9.3", "1.9.3b")
    );
}
