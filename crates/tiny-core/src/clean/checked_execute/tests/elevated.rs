//! Deletion as administrator: one batch before anything else changes, one
//! call to the elevated route, each outcome from what is left on disk, a
//! cancelled dialog that changes nothing, and no other route into or out of
//! it. The fake route removes only files these tests created.

use std::sync::atomic::AtomicUsize;

use super::removal::{planned, trash_row, Sinks};
use super::*;

const CACHES: &str = "system-simulator-caches";

/// Registered ids: the root-owned caches (guarded on `guarded`, gated on
/// Simulator), Rebuilt, Your files and the Trash row.
fn providers() -> Vec<Box<dyn CleanProvider>> {
    let caches = Fixture {
        admin: true,
        guard: Some("guarded"),
        app: Some("Simulator"),
        ..fixture(CACHES)
    };
    let trash = Fixture {
        desktop: false,
        risk: RiskLevel::Destructive,
        empties_trash: true,
        ..fixture("trash")
    };
    vec![
        Box::new(caches),
        Box::new(fixture("user-logs")),
        Box::new(fixture("downloads-old")),
        Box::new(trash),
    ]
}

fn run_admin(
    store: &TempStore,
    plan: &[PlannedItem],
    probe: &dyn AppProbe,
    sinks: &Sinks,
) -> CheckedExecReport {
    run_admin_cancellable(store, plan, probe, sinks, None)
}

fn run_admin_cancellable(
    store: &TempStore,
    plan: &[PlannedItem],
    probe: &dyn AppProbe,
    sinks: &Sinks,
    cancel: Option<&AtomicBool>,
) -> CheckedExecReport {
    let providers = providers();
    let home = std::env::temp_dir();
    let lease = store.store.lease().unwrap();
    let ctx = ExecContext {
        providers: &providers,
        home: Some(&home),
        probe,
        trash: sinks,
        delete: sinks,
        elevated: sinks,
        cancel,
        progress: None,
        policy: &lease,
    };
    execute_checked(plan, &ctx)
}

fn as_admin(root: &Path, name: &str) -> PlannedItem {
    planned(CACHES, root, name, Removal::DeleteAsAdministrator)
}

#[test]
fn deletions_as_administrator_run_first_in_one_call() {
    let root = fixture_root("elevated", &["a", "x", "c", "y"]);
    let plan = [
        planned("user-logs", &root, "a", Removal::Delete),
        as_admin(&root, "x"),
        planned("downloads-old", &root, "c", Removal::MoveToTrash),
        as_admin(&root, "y"),
    ];
    let sinks = Sinks::default();
    let report = run_admin(&TempStore::new(), &plan, &MockChecker::none(), &sinks);
    assert_eq!(
        outcomes(&report),
        [
            ItemOutcome::Deleted,
            ItemOutcome::Deleted,
            ItemOutcome::MovedToTrash,
            ItemOutcome::Deleted
        ]
    );
    assert_eq!(
        sinks.log(),
        ["elevate x y", "delete a", "trash c"],
        "one dialog, before anything else changes"
    );
    assert_eq!(report.bytes_freed, 12, "both caches and the deleted log");
    assert_eq!(report.count(ItemOutcome::frees_space), 3);
    assert_eq!(report.stopped, None);
    let _ = remove_recursive_safe(&root);
}

#[test]
fn each_path_of_a_failed_batch_is_judged_by_what_is_left() {
    let root = fixture_root("elevated-partial", &["a", "x", "y"]);
    let plan = [
        as_admin(&root, "x"),
        as_admin(&root, "y"),
        planned("user-logs", &root, "a", Removal::Delete),
    ];
    let sinks = Sinks {
        fail_elevate: Some(ElevatedError::Failed(
            "rm: y: Operation not permitted".into(),
        )),
        keep_on_elevate: Some("y"),
        ..Default::default()
    };
    let report = run_admin(&TempStore::new(), &plan, &MockChecker::none(), &sinks);
    assert_eq!(
        outcomes(&report),
        [
            ItemOutcome::Deleted,
            ItemOutcome::DeleteFailed {
                detail: "rm: y: Operation not permitted".into(),
                source_present: true,
            },
            ItemOutcome::Deleted,
        ]
    );
    assert_eq!(
        sinks.log(),
        ["elevate x y", "delete a"],
        "the path left behind is never retried, deleted or moved"
    );
    assert_eq!(report.bytes_freed, 8);
    assert!(root.join("y").exists());

    // Reported done, but the path is still there: trust the disk.
    let sinks = Sinks {
        keep_on_elevate: Some("y"),
        ..Default::default()
    };
    let report = run_admin(
        &TempStore::new(),
        &[as_admin(&root, "y")],
        &MockChecker::none(),
        &sinks,
    );
    assert!(matches!(
        &outcomes(&report)[0],
        ItemOutcome::DeleteFailed {
            source_present: true,
            ..
        }
    ));
    assert_eq!(report.bytes_freed, 0);
    let _ = remove_recursive_safe(&root);
}

#[test]
fn a_cancelled_authentication_changes_nothing_and_says_why() {
    let root = fixture_root("auth-cancelled", &["a", "x", "c", "changed"]);
    let changed = as_admin(&root, "changed");
    fs::write(root.join("changed"), b"edited since the scan").unwrap();
    let plan = [
        planned("user-logs", &root, "a", Removal::Delete),
        as_admin(&root, "x"),
        planned("downloads-old", &root, "c", Removal::MoveToTrash),
        changed,
        trash_row(&root),
    ];
    let sinks = Sinks {
        fail_elevate: Some(ElevatedError::Cancelled),
        ..Default::default()
    };
    let report = run_admin(&TempStore::new(), &plan, &MockChecker::none(), &sinks);
    assert_eq!(report.stopped, Some(StopReason::AuthenticationCancelled));
    assert_eq!(
        outcomes(&report),
        vec![ItemOutcome::NotAttempted; 5],
        "nothing changed, not even the item skipped before the dialog"
    );
    assert_eq!(sinks.log(), ["elevate x"], "no other route ran");
    for kept in ["a", "x", "c", "changed", ".Trash/old"] {
        assert!(root.join(kept).exists(), "{kept}");
    }
    assert_eq!(report.bytes_freed, 0);
    let _ = remove_recursive_safe(&root);
}

#[test]
fn a_simulator_tool_found_as_root_skips_the_batch_and_the_run_goes_on() {
    let root = fixture_root("elevated-busy", &["a", "x"]);
    let plan = [
        as_admin(&root, "x"),
        planned("user-logs", &root, "a", Removal::Delete),
    ];
    let sinks = Sinks {
        fail_elevate: Some(ElevatedError::Busy("Simulator".into())),
        ..Default::default()
    };
    let report = run_admin(&TempStore::new(), &plan, &MockChecker::none(), &sinks);
    assert_eq!(
        outcomes(&report),
        [
            ItemOutcome::Skipped(SkipReason::AppRunning("Simulator".into())),
            ItemOutcome::Deleted
        ]
    );
    assert_eq!(sinks.log(), ["elevate x", "delete a"]);
    assert!(root.join("x").exists());
    let _ = remove_recursive_safe(&root);
}

#[test]
fn deletion_as_administrator_keeps_every_guard_and_asks_nothing_when_none_pass() {
    let root = fixture_root(
        "elevated-guards",
        &["kept", "guarded", "changed", "running"],
    );
    fs::create_dir(root.join("real")).unwrap();
    std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
    let store = TempStore::new();
    store.keep(&root.join("kept"), RuleScope::File);
    let changed = as_admin(&root, "changed");
    fs::write(root.join("changed"), b"edited since the scan").unwrap();
    let mut system = as_admin(&root, "kept");
    system.item.path = PathBuf::from("/System/Library/tiny-test-missing");
    let plan = [
        as_admin(&root, "kept"),
        as_admin(&root, "guarded"),
        PlannedItem {
            fingerprint: fingerprint(&root.join("link")).ok(),
            ..as_admin(&root, "link")
        },
        changed,
        system,
    ];
    let sinks = Sinks::default();
    let report = run_admin(&store, &plan, &MockChecker::none(), &sinks);
    let skipped = outcomes(&report);
    assert!(matches!(
        skipped[0],
        ItemOutcome::Skipped(SkipReason::Kept { .. })
    ));
    assert_eq!(
        skipped[1..],
        [
            ItemOutcome::Skipped(SkipReason::ProviderGuard("guarded is protected".into())),
            ItemOutcome::Skipped(SkipReason::Symlink),
            ItemOutcome::Skipped(SkipReason::Changed),
            ItemOutcome::Skipped(SkipReason::ProtectedPath("inside /System".into())),
        ]
    );
    let report = run_admin(
        &TempStore::new(),
        &[as_admin(&root, "running")],
        &MockChecker::with_running(["Simulator"]),
        &sinks,
    );
    assert_eq!(
        outcomes(&report),
        [ItemOutcome::Skipped(SkipReason::AppRunning(
            "Simulator".into()
        ))]
    );
    assert!(sinks.log().is_empty(), "no dialog: {:?}", sinks.log());
    let _ = remove_recursive_safe(&root);
}

#[test]
fn only_categories_that_need_administrator_rights_go_this_way_and_no_other() {
    let root = fixture_root("elevated-mismatch", &["x", "a"]);
    let caches_as = |removal| planned(CACHES, &root, "x", removal);
    let carrying = PlannedItem {
        covers: vec![plan("user-logs", &root, "a").item],
        ..as_admin(&root, "x")
    };
    let plan = [
        caches_as(Removal::Delete),
        caches_as(Removal::MoveToTrash),
        caches_as(Removal::EmptyTrash),
        caches_as(Removal::RunTool(DesktopTool::Prune)),
        planned("user-logs", &root, "a", Removal::DeleteAsAdministrator),
        carrying,
    ];
    let sinks = Sinks::default();
    let report = run_admin(&TempStore::new(), &plan, &MockChecker::none(), &sinks);
    assert_eq!(
        outcomes(&report),
        vec![ItemOutcome::Skipped(SkipReason::ReportOnly); 6]
    );
    assert!(sinks.log().is_empty(), "{:?}", sinks.log());
    assert!(root.join("x").exists() && root.join("a").exists());
    let _ = remove_recursive_safe(&root);
}

/// Answers "not running" to its first question. From the second on it runs
/// `hook`, then answers "running": the batch's last item is skipped by its
/// checks, which return before the ones that look at the cancel flag and
/// the saved rules again. Only the check right before the dialog is left to
/// notice what `hook` changed.
struct SkipsTheLastItem<'a> {
    asked: AtomicUsize,
    hook: Box<dyn Fn() + Send + Sync + 'a>,
}

impl<'a> SkipsTheLastItem<'a> {
    fn new(hook: impl Fn() + Send + Sync + 'a) -> Self {
        Self {
            asked: AtomicUsize::new(0),
            hook: Box::new(hook),
        }
    }
}

impl AppProbe for SkipsTheLastItem<'_> {
    fn probe(&self, _: &str) -> std::result::Result<bool, String> {
        if self.asked.fetch_add(1, Ordering::SeqCst) == 0 {
            return Ok(false);
        }
        (self.hook)();
        Ok(true)
    }
}

#[test]
fn a_run_cancelled_while_the_batch_is_checked_never_reaches_the_dialog() {
    let root = fixture_root("elevated-cancel-late", &["x", "y"]);
    let cancel = AtomicBool::new(false);
    let probe = SkipsTheLastItem::new(|| cancel.store(true, Ordering::Release));
    let plan = [as_admin(&root, "x"), as_admin(&root, "y")];
    let sinks = Sinks::default();
    let report = run_admin_cancellable(&TempStore::new(), &plan, &probe, &sinks, Some(&cancel));
    assert_eq!(report.stopped, Some(StopReason::Cancelled));
    assert_eq!(
        outcomes(&report),
        [
            ItemOutcome::NotAttempted,
            ItemOutcome::Skipped(SkipReason::AppRunning("Simulator".into()))
        ]
    );
    assert!(sinks.log().is_empty(), "no dialog: {:?}", sinks.log());
    assert!(root.join("x").exists() && root.join("y").exists());
    let _ = remove_recursive_safe(&root);
}

#[test]
fn rules_changed_while_the_batch_is_checked_never_reach_the_dialog() {
    let root = fixture_root("elevated-rules-late", &["x", "y"]);
    let store = TempStore::new();
    store.keep(&root.join("elsewhere"), RuleScope::File);
    let x = root.join("x");
    let probe = SkipsTheLastItem::new(|| {
        store.rewrite(|doc| {
            doc.revision += 1;
            doc.next_id += 1;
            doc.rules.push(ProtectionRule {
                id: "r2".into(),
                path: x.clone(),
                scope: RuleScope::File,
            });
        })
    });
    let plan = [as_admin(&root, "x"), as_admin(&root, "y")];
    let sinks = Sinks::default();
    let report = run_admin(&store, &plan, &probe, &sinks);
    assert_eq!(report.stopped, Some(StopReason::ProtectionChanged));
    assert_eq!(
        outcomes(&report),
        [
            ItemOutcome::NotAttempted,
            ItemOutcome::Skipped(SkipReason::AppRunning("Simulator".into()))
        ]
    );
    assert!(sinks.log().is_empty(), "no dialog: {:?}", sinks.log());
    assert!(root.join("x").exists() && root.join("y").exists());
    let _ = remove_recursive_safe(&root);
}

#[test]
fn a_run_cancelled_before_the_batch_starts_never_reaches_the_dialog() {
    let root = fixture_root("elevated-cancel-early", &["x"]);
    let cancel = AtomicBool::new(true);
    let sinks = Sinks::default();
    let report = run_admin_cancellable(
        &TempStore::new(),
        &[as_admin(&root, "x")],
        &MockChecker::none(),
        &sinks,
        Some(&cancel),
    );
    assert_eq!(report.stopped, Some(StopReason::Cancelled));
    assert_eq!(outcomes(&report), [ItemOutcome::NotAttempted]);
    assert!(sinks.log().is_empty(), "no dialog: {:?}", sinks.log());
    let _ = remove_recursive_safe(&root);
}
