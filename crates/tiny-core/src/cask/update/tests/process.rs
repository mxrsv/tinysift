//! `run_updates` through a real `ToolRunner` and a scripted `brew`.

use super::*;
use crate::runner::{ToolLookup, ToolRunner};

fn upgrade_of(dir: &Path, token: &str) -> CaskPlanItem {
    plan_item(CaskAction::Update, token, installed_app(dir, token))
}

fn scripted(tokens: &[&str]) -> ScriptedBrew {
    let brew = ScriptedBrew::new();
    let listed: Vec<_> = tokens.iter().map(|t| (*t, "1.0", "2.0", false)).collect();
    brew.file("outdated.json", &outdated_json(&listed));
    for token in tokens {
        let mut info = simple_cask(token, "2.0", &format!("{token}.app"));
        info["installed"] = serde_json::json!("1.0");
        info["outdated"] = serde_json::json!(true);
        brew.file(&format!("info-{token}.json"), &info_json(&[info]));
    }
    brew
}

#[test]
fn the_upgrade_is_spawned_with_exactly_these_arguments_and_the_batch_goes_in_order() {
    let brew = scripted(&["a", "b"]);
    for token in ["a", "b"] {
        brew.run_script(
            token,
            "echo \"==> Upgrading $0\"\necho \"==> Moving App 'x.app'\"",
        );
    }
    let dir = tempfile::tempdir().unwrap();
    let items = [upgrade_of(dir.path(), "a"), upgrade_of(dir.path(), "b")];
    let runner = ToolRunner::new(ToolLookup::default());
    let path = brew.path();
    let results = run_updates(
        &Brew::new(&path, &runner),
        &items,
        &MockChecker::none(),
        &executable,
        None,
        None,
    );
    assert_eq!(
        results.iter().map(|(_, o)| o).collect::<Vec<_>>(),
        [&RunOutcome::Done, &RunOutcome::Done]
    );
    assert_eq!(
        brew.calls(),
        [
            "outdated --cask --json=v2",
            "info --cask --json=v2 a",
            "upgrade --cask --no-quit a",
            "outdated --cask --json=v2",
            "info --cask --json=v2 b",
            "upgrade --cask --no-quit b",
        ]
    );
}

#[test]
fn a_cancel_during_one_cask_installing_lets_it_finish_and_starts_nothing_after_it() {
    let brew = scripted(&["a", "b"]);
    let finished_a = brew.marker("a-done");
    let started_b = brew.marker("b-started");
    brew.run_script(
        "a",
        &format!(
            "echo '==> Installing Cask a'\necho \"==> Backing App 'a.app' up\"\nsleep 0.8\ntouch '{}'",
            finished_a.display()
        ),
    );
    brew.run_script("b", &format!("touch '{}'", started_b.display()));
    let dir = tempfile::tempdir().unwrap();
    let items = [upgrade_of(dir.path(), "a"), upgrade_of(dir.path(), "b")];
    let runner = ToolRunner::new(ToolLookup::default());
    let path = brew.path();
    let cancel = AtomicBool::new(false);
    let report = |step: CaskProgress| {
        if step.stage == Stage::Installing {
            cancel.store(true, Ordering::Release);
        }
    };
    let results = run_updates(
        &Brew::new(&path, &runner),
        &items,
        &MockChecker::none(),
        &executable,
        Some(&cancel),
        Some(&report),
    );
    assert_eq!(results[0].1, RunOutcome::Done);
    assert_eq!(results[1].1, RunOutcome::NotAttempted);
    assert!(finished_a.exists(), "the first cask was not cut in half");
    assert!(!started_b.exists());
    assert!(
        !brew.calls().iter().any(|c| c.contains(" b")),
        "nothing about b was asked"
    );
}
