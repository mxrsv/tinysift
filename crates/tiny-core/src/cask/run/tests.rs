use std::path::PathBuf;
use std::sync::atomic::Ordering;

use super::*;
use crate::cask::test_support::*;
use crate::runner::test_support::MockRunner;
use crate::runner::{CommandError, CommandOutcome};

const NOWHERE: &str = "/nonexistent/tiny-test/App.app";

fn item(action: CaskAction) -> CaskPlanItem {
    plan_item(action, "ghostty", PathBuf::from(NOWHERE))
}

/// Runs `item` against a runner that answers `args` with `result`, returning
/// the outcome, the stages reported and the calls the runner saw.
fn run(
    item: &CaskPlanItem,
    result: Result<CommandOutcome, CommandError>,
) -> (RunOutcome, Vec<Stage>, Vec<String>) {
    let runner = MockRunner::new().with_output(BREW_PATH, &item.args(), result);
    let mut stages = Vec::new();
    let outcome = run_item(
        &Brew::new(std::path::Path::new(BREW_PATH), &runner),
        item,
        None,
        &mut |stage| stages.push(stage),
    );
    let calls = runner.output_calls.lock().unwrap().clone();
    (outcome, stages, calls)
}

#[test]
fn headings_move_the_stage_and_other_lines_do_not() {
    for (line, stage) in [
        (
            "==> Fetching downloads for: ghostty",
            Some(Stage::Downloading),
        ),
        (
            "==> Fetching dependency downloads",
            Some(Stage::Downloading),
        ),
        (
            "==> Downloading https://example.com/x.dmg",
            Some(Stage::Downloading),
        ),
        ("==> Installing Cask ghostty", Some(Stage::Installing)),
        (
            "==> Moving App 'Ghostty.app' to '/Applications/Ghostty.app'",
            Some(Stage::Installing),
        ),
        (
            "==> Linking Manpage 'ghostty.1' to '/opt/homebrew/share/man/man1/ghostty.1'",
            Some(Stage::Installing),
        ),
        (
            "==> Backing App 'iTerm.app' up to '/x/iTerm.app'",
            Some(Stage::Installing),
        ),
        (
            "==> Removing App '/Applications/iTerm.app'",
            Some(Stage::Installing),
        ),
        (
            "==> Running installer for x with `sudo`",
            Some(Stage::Installing),
        ),
        (
            "  ==> Moving App 'X.app' to '/Applications/X.app'",
            Some(Stage::Installing),
        ),
        (
            "==> Quitting application 'com.mitchellh.ghostty'...",
            Some(Stage::Installing),
        ),
        (
            "==> Purging files for version 1.2.2 of Cask ghostty",
            Some(Stage::Installing),
        ),
        ("✔︎ Cask ghostty (1.3.1)", None),
        (
            "Application 'com.mitchellh.ghostty' quit successfully.",
            None,
        ),
        ("🍺  ghostty was successfully upgraded!", None),
        ("==> Reopening 1 application closed during upgrade:", None),
        ("==> Upgrading 1 outdated package:", None),
        ("==> Upgrading ghostty", None),
        ("==> Verifying checksum for 'Ghostty.dmg'", None),
        ("ghostty was successfully installed!", None),
        ("Downloading without a heading", None),
        ("", None),
    ] {
        assert_eq!(stage_of(line), stage, "{line:?}");
    }
}

#[test]
fn an_install_runs_the_exact_command_and_reports_each_stage_once() {
    let (outcome, stages, calls) = run(
        &item(CaskAction::Install),
        Ok(outcome(
            0,
            "==> Installing Cask ghostty\n==> Moving App 'Ghostty.app'\n==> Linking Manpage\n\
             ghostty was successfully installed!\n",
            "==> Fetching downloads for: ghostty\n✔︎ Cask ghostty (1.3.1)\n",
        )),
    );
    assert_eq!(outcome, RunOutcome::Done);
    assert_eq!(
        stages,
        [Stage::Working, Stage::Downloading, Stage::Installing]
    );
    assert_eq!(calls, [format!("{BREW_PATH} install --cask ghostty")]);
}

#[test]
fn an_update_runs_upgrade_for_that_cask_alone() {
    let (outcome, stages, calls) = run(&item(CaskAction::Update), Ok(outcome(0, "", "")));
    assert_eq!(outcome, RunOutcome::Done);
    assert_eq!(stages, [Stage::Working]);
    assert_eq!(
        calls,
        [format!("{BREW_PATH} upgrade --cask --no-quit ghostty")]
    );
    assert_eq!(
        item(CaskAction::Update).command(),
        "brew upgrade --cask --no-quit ghostty"
    );
    assert_eq!(
        item(CaskAction::Install).command(),
        "brew install --cask ghostty"
    );
}

#[test]
fn a_failed_run_says_what_went_wrong() {
    let failed = |stderr: &str| run(&item(CaskAction::Install), Ok(outcome(1, "", stderr))).0;
    assert_eq!(
        failed("Error: ghostty: The pkg artifact requires sudo, but HOMEBREW_NO_SUDO is set."),
        RunOutcome::Failed(CaskFailure::NeedsAdministrator)
    );
    assert!(matches!(
        failed("curl: (6) Could not resolve host: release.files.ghostty.org"),
        RunOutcome::Failed(CaskFailure::Network { .. })
    ));
    assert_eq!(
        failed("Error: Cask 'ghostty' is unavailable: No Cask with this name exists."),
        RunOutcome::Failed(CaskFailure::CaskNotFound {
            token: "ghostty".into()
        })
    );
    assert!(matches!(
        failed("Error: something else broke"),
        RunOutcome::Failed(CaskFailure::BrewFailed {
            exit_code: Some(1),
            ..
        })
    ));
}

#[test]
fn runner_errors_end_the_run_with_a_typed_failure() {
    let ended = |error| run(&item(CaskAction::Install), Err(error)).0;
    assert_eq!(
        ended(CommandError::NotFound(BREW_PATH.into())),
        RunOutcome::Failed(CaskFailure::HomebrewMissing)
    );
    assert_eq!(
        ended(CommandError::Timeout {
            bin: BREW_PATH.into(),
            timeout_ms: 1_800_000
        }),
        RunOutcome::Failed(CaskFailure::TimedOut)
    );
}

#[test]
fn an_exit_of_zero_that_changed_nothing_is_not_reported_done() {
    let said = |action, stderr: &str| run(&item(action), Ok(outcome(0, "", stderr))).0;
    assert_eq!(
        said(
            CaskAction::Install,
            "Warning: Cask 'ghostty' is already installed."
        ),
        RunOutcome::Failed(CaskFailure::AlreadyInstalled {
            token: "ghostty".into()
        })
    );
    assert_eq!(
        said(
            CaskAction::Update,
            "Warning: Not upgrading ghostty, the latest version is already installed"
        ),
        RunOutcome::Failed(CaskFailure::NotOutdated {
            token: "ghostty".into()
        })
    );
    assert_eq!(
        said(CaskAction::Update, "Warning: unrelated"),
        RunOutcome::Done
    );
}

#[test]
fn what_brew_says_counts_on_either_stream_and_in_any_line() {
    let said = |stdout: &str, stderr: &str| {
        run(&item(CaskAction::Install), Ok(outcome(0, stdout, stderr))).0
    };
    let already = RunOutcome::Failed(CaskFailure::AlreadyInstalled {
        token: "ghostty".into(),
    });
    assert_eq!(
        said("Warning: ghostty is already installed.\n", ""),
        already
    );
    assert_eq!(
        said("", "==> x\nWarning: Cask 'ghostty' is already installed.\n"),
        already
    );
    let manual = "Cask ghostty only provides a manual installer. To run it:\n  open /x\n";
    assert_eq!(
        said(manual, ""),
        RunOutcome::Failed(CaskFailure::ManualInstaller {
            token: "ghostty".into()
        })
    );
}

#[test]
fn a_cask_that_needs_a_password_is_refused_without_spawning_brew() {
    let mut needy = item(CaskAction::Install);
    needy.blocked = Some(CaskFailure::NeedsAdministrator);
    let (outcome, stages, calls) = run(&needy, Ok(outcome(0, "", "")));
    assert_eq!(outcome, RunOutcome::Failed(CaskFailure::NeedsAdministrator));
    assert!(stages.is_empty() && calls.is_empty());
}

#[test]
fn an_install_over_an_existing_app_is_refused_without_spawning_brew() {
    let dir = tempfile::tempdir().unwrap();
    let app = dir.path().join("Ghostty.app");
    std::fs::create_dir(&app).unwrap();
    let taken = plan_item(CaskAction::Install, "ghostty", app.clone());
    let (outcome, _, calls) = run(&taken, Ok(outcome(0, "", "")));
    assert_eq!(
        outcome,
        RunOutcome::Failed(CaskFailure::AppExists {
            path: app.to_string_lossy().into_owned()
        })
    );
    assert!(calls.is_empty());
    // An update replaces the app that is there, by definition.
    let update = plan_item(CaskAction::Update, "ghostty", app);
    assert_eq!(run(&update, Ok(outcome_ok())).0, RunOutcome::Done);
}

fn outcome_ok() -> CommandOutcome {
    outcome(0, "", "")
}

#[test]
fn a_raised_flag_stops_the_run_before_brew_starts() {
    let item = item(CaskAction::Install);
    let runner = MockRunner::new().with_output(BREW_PATH, &item.args(), Ok(outcome_ok()));
    let cancel = std::sync::atomic::AtomicBool::new(false);
    cancel.store(true, Ordering::Release);
    let outcome = run_item(
        &Brew::new(std::path::Path::new(BREW_PATH), &runner),
        &item,
        Some(&cancel),
        &mut |_| {},
    );
    assert_eq!(outcome, RunOutcome::Cancelled);
    assert!(runner.output_calls.lock().unwrap().is_empty());
}

#[test]
fn unsupported_no_quit_fails_without_retrying_an_unsafe_command() {
    let (outcome, _, calls) = run(
        &item(CaskAction::Update),
        Ok(outcome(1, "", "Error: invalid option: --no-quit")),
    );
    assert!(matches!(
        outcome,
        RunOutcome::Failed(CaskFailure::BrewFailed { .. })
    ));
    assert_eq!(
        calls,
        [format!("{BREW_PATH} upgrade --cask --no-quit ghostty")]
    );
}
