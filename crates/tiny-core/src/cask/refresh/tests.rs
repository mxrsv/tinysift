use std::ffi::OsString;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::*;
use crate::cask::brew::{
    isolated_brew, locate_brew, HomebrewStatus, BREW_ENV, BREW_PATH as SEARCH,
};
use crate::cask::test_support::{outcome, BREW_PATH};
use crate::runner::test_support::MockRunner;
use crate::runner::{CommandError, ToolLookup, ToolRunner};

fn refresh_with(
    result: Result<crate::runner::CommandOutcome, CommandError>,
) -> (MockRunner, Result<IndexRefresh, CaskFailure>) {
    let runner = MockRunner::new().with_output(BREW_PATH, &["update"], result);
    let refreshed = refresh_index(&Brew::new(Path::new(BREW_PATH), &runner));
    (runner, refreshed)
}

#[test]
fn a_finished_update_is_refreshed_and_runs_only_brew_update() {
    let (runner, refreshed) = refresh_with(Ok(outcome(0, "Already up-to-date.\n", "")));
    assert_eq!(refreshed, Ok(IndexRefresh::Refreshed));
    assert_eq!(
        *runner.output_calls.lock().unwrap(),
        [format!("{BREW_PATH} update")]
    );
}

#[test]
fn another_brew_holding_a_lock_defers_the_refresh() {
    // Homebrew 7's `utils/lock.sh` and `cmd/update.sh`, word for word.
    for stderr in [
        "Error: Another `brew update` process is already running.\n\
         Please wait for it to finish or terminate it to continue.\n",
        "Error: Cannot lock cask.jws.json for download!\n",
    ] {
        let (_, refreshed) = refresh_with(Ok(outcome(1, "", stderr)));
        assert_eq!(refreshed, Ok(IndexRefresh::Deferred), "{stderr}");
    }
}

#[test]
fn no_network_ends_the_refresh_without_an_error() {
    let (_, refreshed) = refresh_with(Ok(outcome(
        1,
        "",
        "fatal: unable to access 'https://github.com/Homebrew/brew/': Could not resolve host: github.com\n\
         Error: Fetching /opt/homebrew failed!\n",
    )));
    assert!(
        matches!(
            refreshed,
            Ok(IndexRefresh::Ended(CaskFailure::Network { .. }))
        ),
        "{refreshed:?}"
    );
}

#[test]
fn a_timeout_ends_the_refresh_without_an_error() {
    let (_, refreshed) = refresh_with(Err(CommandError::Timeout {
        bin: BREW_PATH.into(),
        timeout_ms: 1,
    }));
    assert_eq!(
        refreshed,
        Ok(IndexRefresh::Ended(CaskFailure::TimedOut { running: true }))
    );
}

#[test]
fn any_other_failure_of_homebrew_ends_the_refresh_without_an_error() {
    let (_, refreshed) = refresh_with(Ok(outcome(
        1,
        "",
        "Error: Your /opt/homebrew HEAD is not a descendant of main!\n",
    )));
    assert!(
        matches!(
            refreshed,
            Ok(IndexRefresh::Ended(CaskFailure::BrewFailed { .. }))
        ),
        "{refreshed:?}"
    );
}

#[test]
fn only_a_missing_homebrew_is_an_error() {
    let (_, refreshed) = refresh_with(Err(CommandError::NotFound(BREW_PATH.into())));
    assert_eq!(refreshed, Err(CaskFailure::HomebrewMissing));
}

#[test]
fn the_refresh_runs_with_the_fixed_brew_environment() {
    // A `brew` that refreshes only when it sees exactly `BREW_ENV`, `HOME` and
    // the fixed `PATH`, run through the same isolated lookup as the app's.
    let prefix = tempfile::tempdir().unwrap();
    let bin = prefix.path().join("brew");
    let checks: Vec<String> = BREW_ENV
        .iter()
        .map(|(key, value)| format!("[ \"${key}\" = '{value}' ] || exit 7"))
        .collect();
    let script = format!(
        "#!/bin/sh\n[ \"$1\" = update ] || exit 9\n{}\n[ \"$PATH\" = '{SEARCH}' ] || exit 8\n\
         [ \"$HOME\" = /Users/someone ] || exit 6\n[ -z \"$HOMEBREW_GITHUB_API_TOKEN\" ] || exit 5\n",
        checks.join("\n")
    );
    std::fs::write(&bin, script).unwrap();
    std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
    let lookup = isolated_brew(
        ToolLookup::new(Some(OsString::from(prefix.path())), Vec::new()),
        Some(PathBuf::from("/Users/someone")),
    );
    let HomebrewStatus::Present(found) = locate_brew(&lookup) else {
        panic!("the fake brew is found through the search path");
    };
    let runner = ToolRunner::new(lookup);
    assert_eq!(
        refresh_index(&Brew::new(&found, &runner)),
        Ok(IndexRefresh::Refreshed)
    );
}
