//! End-to-end smoke tests for `tiny clean`. We only assert on outputs that
//! must hold regardless of what's actually on disk, so the suite stays
//! green on any developer machine and in CI.

use assert_cmd::Command;
use predicates::prelude::*;

#[test]
fn dry_run_with_all_includes_succeeds() {
    Command::cargo_bin("tiny")
        .unwrap()
        .args([
            "clean",
            "--dry-run",
            "--include-review",
            "--include-destructive",
        ])
        .assert()
        .success()
        .stdout(predicate::str::contains("Cleanup candidates"));
}

#[test]
fn unknown_category_fails_with_clear_message() {
    Command::cargo_bin("tiny")
        .unwrap()
        .args(["clean", "--category", "definitely-not-a-category"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("unknown --category"));
}

#[test]
fn idle_days_zero_is_rejected() {
    Command::cargo_bin("tiny")
        .unwrap()
        .args(["clean", "--dry-run", "--idle-days", "0"])
        .assert()
        .failure()
        .stderr(predicate::str::contains("--idle-days"));
}
