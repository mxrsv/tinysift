//! SF2: right before each move, a file that changed since the scan (size or
//! modification time) or an item whose owning app runs is skipped with a
//! cause; an unchanged one goes. A folder keeps its identity check only:
//! caches churn, and a churning folder must not be skipped. Temp dirs only.

use super::*;
use crate::clean::failure_cause::FailureCause;
use std::time::{Duration, UNIX_EPOCH};

/// A fixed past time, so a later write shows on any clock granularity.
fn age(path: &Path) {
    let past = UNIX_EPOCH + Duration::from_secs(1_000_000);
    fs::File::open(path).unwrap().set_modified(past).unwrap();
}

fn run_one(
    item: &PlannedItem,
    provider: Fixture,
    probe: &MockChecker,
) -> (CheckedExecReport, usize) {
    let trash = FakeTrash::default();
    let report = run(
        std::slice::from_ref(item),
        vec![Box::new(provider)],
        probe,
        &trash,
        None,
    );
    let moved = trash.moved.lock().unwrap().len();
    (report, moved)
}

fn skipped_for(report: &CheckedExecReport, reason: SkipReason, cause: FailureCause) {
    assert_eq!(outcomes(report), vec![ItemOutcome::Skipped(reason)]);
    assert_eq!(
        report.results[0].failure.as_ref().map(|f| f.cause),
        Some(cause)
    );
}

#[test]
fn a_file_changed_in_size_or_only_in_mtime_since_the_scan_is_skipped() {
    let root = fixture_root("recheck-file", &["grew", "touched"]);
    age(&root.join("grew"));
    age(&root.join("touched"));
    let grew = plan("logs", &root, "grew");
    let touched = plan("logs", &root, "touched");
    fs::write(root.join("grew"), b"12345678").unwrap();
    // Same length, new modification time.
    fs::write(root.join("touched"), b"1234").unwrap();
    for item in [grew, touched] {
        let (report, moved) = run_one(&item, fixture("logs"), &MockChecker::none());
        skipped_for(&report, SkipReason::Changed, FailureCause::ChangedSinceScan);
        assert_eq!(moved, 0);
    }
    let _ = remove_recursive_safe(&root);
}

#[test]
fn an_item_whose_owning_app_runs_is_skipped_as_in_use() {
    let root = fixture_root("recheck-app", &["cache"]);
    let item = plan("browser", &root, "cache");
    let mut provider = fixture("browser");
    provider.app = Some("Some Browser");
    let (report, moved) = run_one(
        &item,
        provider,
        &MockChecker::with_running(["Some Browser"]),
    );
    skipped_for(
        &report,
        SkipReason::AppRunning("Some Browser".into()),
        FailureCause::InUse,
    );
    assert_eq!(moved, 0);
    let _ = remove_recursive_safe(&root);
}

#[test]
fn an_unchanged_file_and_folder_with_an_idle_app_are_moved() {
    let root = fixture_root("recheck-ok", &["file"]);
    fs::create_dir(root.join("dir")).unwrap();
    fs::write(root.join("dir/inner"), b"1234").unwrap();
    let items = [
        plan("browser", &root, "file"),
        plan("browser", &root, "dir"),
    ];
    let mut provider = fixture("browser");
    provider.app = Some("Some Browser");
    let trash = FakeTrash::default();
    let report = run(
        &items,
        vec![Box::new(provider)],
        &MockChecker::none(),
        &trash,
        None,
    );
    assert_eq!(outcomes(&report), vec![ItemOutcome::MovedToTrash; 2]);
    assert_eq!(trash.moved.lock().unwrap().len(), 2);
    let _ = remove_recursive_safe(&root);
}
