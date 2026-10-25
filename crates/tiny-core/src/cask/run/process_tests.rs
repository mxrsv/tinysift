//! `run_item` through a real `ToolRunner` and a scripted `brew`: real pipes,
//! real signals. These pin what the fake-runner tests cannot see: the order of
//! stages across stdout and stderr, exit codes, and what a cancel does at each
//! stage.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::*;
use crate::cask::test_support::*;
use crate::runner::{ToolLookup, ToolRunner};

fn run(
    brew: &ScriptedBrew,
    item: &CaskPlanItem,
    cancel: Option<&AtomicBool>,
    cancel_at: Option<Stage>,
) -> (RunOutcome, Vec<Stage>, Duration) {
    let runner = ToolRunner::new(ToolLookup::default());
    let path = brew.path();
    let mut stages = Vec::new();
    let started = Instant::now();
    let outcome = run_item(&Brew::new(&path, &runner), item, cancel, &mut |stage| {
        stages.push(stage);
        if Some(stage) == cancel_at {
            cancel.unwrap().store(true, Ordering::Release);
        }
    });
    (outcome, stages, started.elapsed())
}

fn install(dir: &Path) -> CaskPlanItem {
    plan_item(CaskAction::Install, "ghostty", dir.join("Ghostty.app"))
}

#[test]
fn stages_follow_the_headings_across_both_pipes_in_the_order_they_were_printed() {
    let brew = ScriptedBrew::new();
    brew.run_script(
        "ghostty",
        "echo '==> Fetching downloads for: ghostty' >&2\nsleep 0.2\n\
         echo '==> Installing Cask ghostty'\necho \"==> Moving App 'Ghostty.app'\"\n\
         echo 'ghostty was successfully installed!'",
    );
    let dir = tempfile::tempdir().unwrap();
    let (outcome, stages, _) = run(&brew, &install(dir.path()), None, None);
    assert_eq!(outcome, RunOutcome::Done);
    assert_eq!(
        stages,
        [Stage::Working, Stage::Downloading, Stage::Installing]
    );
    assert_eq!(brew.calls(), ["install --cask ghostty"]);
}

#[test]
fn the_exit_code_decides_and_brews_words_are_classified() {
    let dir = tempfile::tempdir().unwrap();
    for (body, expected) in [
        (
            "echo 'Error: Download failed on Cask x with message: curl: (6) Could not resolve host: h' >&2\nexit 1",
            "network",
        ),
        ("echo 'Error: something broke' >&2\nexit 3", "exit 3"),
        (
            "echo \"Warning: Cask 'ghostty' is already installed.\" >&2\nexit 0",
            "already installed",
        ),
        ("exit 0", "done"),
    ] {
        let brew = ScriptedBrew::new();
        brew.run_script("ghostty", body);
        let (outcome, _, _) = run(&brew, &install(dir.path()), None, None);
        match (expected, outcome) {
            ("network", RunOutcome::Failed(CaskFailure::Network { .. })) => {}
            (
                "exit 3",
                RunOutcome::Failed(CaskFailure::BrewFailed {
                    exit_code: Some(3), ..
                }),
            ) => {}
            ("already installed", RunOutcome::Failed(CaskFailure::AlreadyInstalled { .. })) => {}
            ("done", RunOutcome::Done) => {}
            (expected, other) => panic!("expected {expected}, got {other:?}"),
        }
    }
}

#[test]
fn a_cancel_during_the_download_interrupts_brew_at_once() {
    let brew = ScriptedBrew::new();
    let late = brew.marker("reached-the-end");
    brew.run_script(
        "ghostty",
        &format!(
            "echo '==> Fetching downloads for: ghostty' >&2\nsleep 30\ntouch '{}'",
            late.display()
        ),
    );
    let dir = tempfile::tempdir().unwrap();
    let cancel = AtomicBool::new(false);
    let (outcome, stages, took) = run(
        &brew,
        &install(dir.path()),
        Some(&cancel),
        Some(Stage::Downloading),
    );
    assert_eq!(outcome, RunOutcome::Cancelled);
    assert_eq!(stages, [Stage::Working, Stage::Downloading]);
    assert!(took < Duration::from_secs(4), "{took:?}");
    assert!(!late.exists(), "brew was interrupted, not left to finish");
}

#[test]
fn a_cancel_once_brew_is_installing_waits_for_that_cask_to_finish() {
    let brew = ScriptedBrew::new();
    let done = brew.marker("finished-installing");
    brew.run_script(
        "ghostty",
        &format!(
            // The pause keeps the two pipes' lines in the order printed.
            "echo '==> Fetching downloads for: ghostty' >&2\nsleep 0.2\n\
             echo '==> Installing Cask ghostty'\necho \"==> Moving App 'Ghostty.app'\"\n\
             sleep 0.8\ntouch '{}'\necho 'ghostty was successfully installed!'",
            done.display()
        ),
    );
    let dir = tempfile::tempdir().unwrap();
    let cancel = AtomicBool::new(false);
    let (outcome, stages, took) = run(
        &brew,
        &install(dir.path()),
        Some(&cancel),
        Some(Stage::Installing),
    );
    assert_eq!(outcome, RunOutcome::Done, "the install was not cut in half");
    assert!(done.exists());
    assert!(took >= Duration::from_millis(700), "{took:?}");
    assert_eq!(
        stages,
        [Stage::Working, Stage::Downloading, Stage::Installing]
    );
    assert!(
        cancel.load(Ordering::Acquire),
        "the caller's flag stays raised for what follows"
    );
}

#[test]
fn a_flag_raised_before_brew_starts_spawns_nothing() {
    let brew = ScriptedBrew::new();
    brew.run_script("ghostty", "exit 0");
    let dir = tempfile::tempdir().unwrap();
    let cancel = AtomicBool::new(false);
    let (outcome, stages, _) = run(
        &brew,
        &install(dir.path()),
        Some(&cancel),
        Some(Stage::Working),
    );
    assert_eq!(outcome, RunOutcome::Cancelled);
    assert_eq!(stages, [Stage::Working]);
    assert!(brew.calls().is_empty());
}

#[test]
fn a_callback_that_panics_ends_the_run_and_the_watcher_instead_of_hanging() {
    let brew = ScriptedBrew::new();
    brew.run_script(
        "ghostty",
        "echo '==> Fetching downloads for: ghostty' >&2\nsleep 0.3\necho '==> Installing Cask ghostty'",
    );
    let dir = tempfile::tempdir().unwrap();
    let item = install(dir.path());
    let cancel = AtomicBool::new(false);
    let runner = ToolRunner::new(ToolLookup::default());
    let path = brew.path();
    let started = Instant::now();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_item(
            &Brew::new(&path, &runner),
            &item,
            Some(&cancel),
            &mut |stage| {
                if stage == Stage::Downloading {
                    panic!("a disposable panic in the caller's callback");
                }
            },
        )
    }));
    assert!(outcome.is_err(), "the panic reaches the caller");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}
