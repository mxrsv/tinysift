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

/// `<home>/Documents/<name>/<manifest>`, `days` old, with `<output>/file`.
fn project(home: &std::path::Path, name: &str, manifest: &str, output: &str, days: u64) {
    let dir = home.join("Documents").join(name);
    std::fs::create_dir_all(dir.join(output)).unwrap();
    std::fs::write(dir.join(output).join("file"), b"x").unwrap();
    let manifest = dir.join(manifest);
    std::fs::write(&manifest, b"{}").unwrap();
    let when = std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86_400);
    std::fs::File::open(&manifest)
        .unwrap()
        .set_modified(when)
        .unwrap();
}

#[test]
fn build_output_lists_only_idle_projects_by_default() {
    // Outside any git work tree, so no git runs; HOME scopes the search roots.
    let home = tempfile::tempdir().unwrap();
    project(home.path(), "idle-web", "package.json", "node_modules", 40);
    project(home.path(), "active-web", "package.json", "node_modules", 2);
    project(home.path(), "idle-crate", "Cargo.toml", "target", 40);
    project(home.path(), "active-crate", "Cargo.toml", "target", 2);
    let output = Command::cargo_bin("tiny")
        .unwrap()
        .env("HOME", home.path())
        .args([
            "clean",
            "--dry-run",
            "--include-review",
            "--category",
            "rust-targets",
            "--category",
            "node-modules",
        ])
        .assert()
        .success()
        .get_output()
        .stdout
        .clone();
    let stdout = String::from_utf8(output).unwrap();
    for expected in [
        "node_modules (idle)",
        "Rust target/ (idle)",
        "idle-web/node_modules",
        "idle-crate/target",
    ] {
        assert!(stdout.contains(expected), "{expected} missing:\n{stdout}");
    }
    assert!(
        !stdout.contains("active-"),
        "active project listed:\n{stdout}"
    );
}

#[test]
fn simulator_categories_list_read_only() {
    Command::cargo_bin("tiny")
        .unwrap()
        .args([
            "clean",
            "--dry-run",
            "--category",
            "system-simulator-caches",
            "--category",
            "simulator-runtimes",
            "--category",
            "simulator-runtime-images",
        ])
        .assert()
        .success()
        .stdout(
            predicate::str::contains("dry-run")
                .or(predicate::str::contains("no cleanup candidates")),
        );
}
