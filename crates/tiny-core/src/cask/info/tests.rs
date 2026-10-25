use std::path::{Path, PathBuf};

use serde_json::json;

use super::*;
use crate::cask::test_support::*;
use crate::runner::test_support::MockRunner;
use crate::runner::{CommandError, CommandOutcome};

/// Visual Studio Code as `brew info` prints it on a Mac that has it: the
/// app updated itself past what Homebrew recorded.
const VS_CODE: &str = r#"{"formulae":[],"casks":[{
  "token":"visual-studio-code","name":["Microsoft Visual Studio Code","VS Code"],
  "url":"https://update.code.visualstudio.com/1.142.0/darwin-arm64/stable",
  "version":"1.142.0","installed":"1.100.1","bundle_short_version":"1.139.1",
  "outdated":true,"auto_updates":true,
  "artifacts":[
    {"uninstall":[{"launchctl":"com.microsoft.VSCode.ShipIt","quit":"com.microsoft.VSCode"}]},
    {"app":["Visual Studio Code.app"],"target":"/Applications/Visual Studio Code.app"},
    {"binary":["/Applications/Visual Studio Code.app/Contents/Resources/app/bin/code"],
     "target":"/opt/homebrew/bin/code"},
    {"zap":[{"trash":["~/.vscode","~/Library/Caches/com.microsoft.VSCode"]}]}
  ]}]}"#;

fn infos(json: &str) -> Vec<CaskInfo> {
    parse_cask_info(json).expect("fixture parses")
}

#[test]
fn an_installed_cask_reports_its_versions_host_and_app() {
    let info = &infos(VS_CODE)[0];
    assert_eq!(info.token, "visual-studio-code");
    assert_eq!(info.name, "Microsoft Visual Studio Code");
    assert_eq!(info.version, "1.142.0");
    assert_eq!(info.installed.as_deref(), Some("1.100.1"));
    assert_eq!(info.from_version(), Some("1.139.1"), "the app on disk wins");
    assert_eq!(
        info.download_host.as_deref(),
        Some("update.code.visualstudio.com")
    );
    assert!(info.outdated);
    assert_eq!(info.elevation, Elevation::default());
    assert_eq!(
        info.apps,
        [PathBuf::from("/Applications/Visual Studio Code.app")]
    );
}

#[test]
fn a_cask_that_is_not_installed_has_no_installed_version() {
    let json = info_json(&[simple_cask("rectangle", "2.0.3", "Rectangle.app")]);
    let info = &infos(&json)[0];
    assert_eq!(info.installed, None);
    assert_eq!(info.from_version(), None);
    assert_eq!(info.name, "rectangle", "falls back to the token");
    assert!(!info.outdated);
}

#[test]
fn an_app_without_a_target_lands_in_applications() {
    let json = info_json(&[cask("x", "1", json!([{"app": ["X.app"]}]))]);
    assert_eq!(infos(&json)[0].apps, [Path::new("/Applications/X.app")]);
}

#[test]
fn the_elevation_is_read_from_the_artifacts() {
    let json = info_json(&[cask("x", "1", json!([{"pkg": ["X.pkg"]}]))]);
    assert!(infos(&json)[0].elevation.install);
}

#[test]
fn a_build_number_after_a_comma_is_not_shown() {
    for (version, shown) in [
        ("0.4.26,4", "0.4.26"),
        ("4.2.1,20260531.1", "4.2.1"),
        ("1.142.0", "1.142.0"),
        ("", ""),
    ] {
        assert_eq!(display_version(version), shown, "{version:?}");
    }
}

#[test]
fn the_download_host_is_read_from_the_url_alone() {
    for (url, host) in [
        (
            "https://release.files.ghostty.org/1.3.1/Ghostty.dmg",
            Some("release.files.ghostty.org"),
        ),
        (
            "http://Example.COM:8080/a?b=https://other.test/",
            Some("example.com"),
        ),
        ("https://user:pw@dl.example.com/x", Some("dl.example.com")),
        ("https://example.com", Some("example.com")),
        ("ftp://example.com/x", None),
        ("example.com/x", None),
        ("https:///x", None),
    ] {
        assert_eq!(host_of(url).as_deref(), host, "{url}");
    }
}

#[test]
fn only_bare_core_cask_tokens_are_valid() {
    for token in [
        "visual-studio-code",
        "lm-studio",
        "iterm2",
        "font-fira-code",
        "a",
        "x.y_z+1@beta",
    ] {
        assert!(valid_token(token), "{token}");
    }
    for token in [
        "",
        "-x",
        "--force",
        "Rectangle",
        "user/tap/cask",
        "../x",
        "a b",
        "a;b",
        "a\nb",
        "é",
        &"a".repeat(MAX_TOKEN_LEN + 1),
    ] {
        assert!(!valid_token(token), "{token:?}");
    }
}

fn brew_info_args(tokens: &[&str]) -> String {
    format!("{BREW_PATH} info --cask --json=v2 {}", tokens.join(" "))
}

fn run_info(runner: &MockRunner, tokens: &[&str]) -> Result<Vec<CaskInfo>, CaskFailure> {
    read_cask_info(&Brew::new(Path::new(BREW_PATH), runner), tokens)
}

#[test]
fn info_runs_brew_with_an_argument_list_and_reads_every_cask() {
    let json = info_json(&[
        simple_cask("ghostty", "1.3.1", "Ghostty.app"),
        simple_cask("rectangle", "2.0.3", "Rectangle.app"),
    ]);
    let runner = MockRunner::new().with_exit(
        BREW_PATH,
        &["info", "--cask", "--json=v2", "ghostty", "rectangle"],
        0,
        &json,
    );
    let casks = run_info(&runner, &["ghostty", "rectangle"]).unwrap();
    assert_eq!(
        casks.iter().map(|c| c.token.as_str()).collect::<Vec<_>>(),
        ["ghostty", "rectangle"]
    );
    assert_eq!(
        *runner.output_calls.lock().unwrap(),
        [brew_info_args(&["ghostty", "rectangle"])]
    );
}

#[test]
fn a_token_that_could_be_a_flag_or_a_tap_never_reaches_brew() {
    let runner = MockRunner::new();
    for bad in ["--force", "user/tap/cask", "Rectangle", ""] {
        let failure = run_info(&runner, &["ghostty", bad]).unwrap_err();
        assert_eq!(failure, CaskFailure::NotInCatalogue { token: bad.into() });
    }
    assert!(runner.output_calls.lock().unwrap().is_empty());
    assert_eq!(run_info(&runner, &[]), Ok(Vec::new()));
    assert!(runner.output_calls.lock().unwrap().is_empty());
}

#[test]
fn info_failures_are_typed() {
    let args = ["info", "--cask", "--json=v2", "ghostty"];
    let failed = |result| {
        run_info(
            &MockRunner::new().with_output(BREW_PATH, &args, result),
            &["ghostty"],
        )
    };
    assert_eq!(
        failed(Ok(CommandOutcome {
            status: Some(1),
            stdout: String::new(),
            stderr: "Error: Cask 'ghostty' is unavailable: No Cask with this name exists.".into(),
        })),
        Err(CaskFailure::CaskNotFound {
            token: "ghostty".into()
        })
    );
    assert_eq!(
        failed(Err(CommandError::NotFound(BREW_PATH.into()))),
        Err(CaskFailure::HomebrewMissing)
    );
    assert_eq!(
        failed(Err(CommandError::Timeout {
            bin: BREW_PATH.into(),
            timeout_ms: 60_000
        })),
        Err(CaskFailure::TimedOut { running: false })
    );
    let garbled = failed(Ok(CommandOutcome {
        status: Some(0),
        stdout: "not json".into(),
        stderr: String::new(),
    }));
    assert!(matches!(
        garbled,
        Err(CaskFailure::BrewFailed {
            exit_code: None,
            ..
        })
    ));
}

#[test]
fn a_cask_with_no_version_for_this_mac_is_not_offered() {
    let mut cask = simple_cask("old", "1", "Old.app");
    cask["version"] = json!(null);
    let failure = parse_cask_info(&info_json(&[cask])).unwrap_err();
    assert!(failure.detail().unwrap().contains("old has no version"));
}
