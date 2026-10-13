//! The saved "always keep" rules at execution time. FakeTrash only: a path
//! these rules guard must never reach it.

use std::os::unix::fs::PermissionsExt;

use super::*;
use crate::clean::protection::INTERNAL_ID_PREFIX;

/// Runs a closure after each successful move, so a test can change the
/// world between two moves.
struct HookedTrash<'a> {
    inner: FakeTrash,
    after_move: Box<dyn Fn() + Send + Sync + 'a>,
}

impl Trash for HookedTrash<'_> {
    fn move_to_trash(&self, path: &Path) -> std::result::Result<(), TrashError> {
        self.inner.move_to_trash(path)?;
        (self.after_move)();
        Ok(())
    }
}

/// A probe that runs a closure instead of looking at any app.
struct HookedProbe<'a>(Box<dyn Fn() + Send + Sync + 'a>);

impl AppProbe for HookedProbe<'_> {
    fn probe(&self, _: &str) -> std::result::Result<bool, String> {
        (self.0)();
        Ok(false)
    }
}

fn logs() -> Vec<Box<dyn CleanProvider>> {
    vec![Box::new(fixture("logs"))]
}

fn moved(trash: &FakeTrash) -> Vec<PathBuf> {
    trash.moved.lock().unwrap().clone()
}

fn three(label: &str) -> (PathBuf, [PlannedItem; 3]) {
    let root = fixture_root(label, &["a", "b", "c"]);
    let items = [
        plan("logs", &root, "a"),
        plan("logs", &root, "b"),
        plan("logs", &root, "c"),
    ];
    (root, items)
}

#[test]
fn kept_paths_and_everything_that_would_carry_one_never_reach_trash() {
    let root = fixture_root("kept", &["file", "free"]);
    for dir in ["keep/sub", "holder", "folder/child"] {
        fs::create_dir_all(root.join(dir)).unwrap();
    }
    let store = TempStore::new();
    store.keep(&root.join("file"), RuleScope::File);
    store.keep(&root.join("keep"), RuleScope::Directory);
    // Not discovered, not even present: only the rule names it.
    store.keep(&root.join("holder/inner/precious"), RuleScope::File);
    // A file rule covers exactly its path, not what lies below it.
    store.keep(&root.join("folder"), RuleScope::File);
    let items = [
        plan("logs", &root, "file"),
        plan("logs", &root, "keep/sub"),
        plan("logs", &root, "holder"),
        plan("logs", &root, "folder/child"),
        plan("logs", &root, "free"),
    ];
    let trash = FakeTrash::default();
    let report = run_with(&store, &items, logs(), &MockChecker::none(), &trash, None);
    let kept = |name: &str| SkipReason::Kept {
        rule_path: root.join(name),
    };
    assert_eq!(
        outcomes(&report),
        vec![
            ItemOutcome::Skipped(kept("file")),
            ItemOutcome::Skipped(kept("keep")),
            ItemOutcome::Skipped(SkipReason::HoldsKept {
                rule_path: root.join("holder/inner/precious")
            }),
            ItemOutcome::MovedToTrash,
            ItemOutcome::MovedToTrash,
        ]
    );
    assert_eq!(report.stopped, None);
    assert_eq!(
        moved(&trash),
        [root.join("folder/child"), root.join("free")]
    );
    let _ = remove_recursive_safe(&root);
}

#[test]
fn a_candidate_carrying_a_kept_path_of_a_covered_item_is_refused() {
    let root = fixture_root("covered", &[]);
    fs::create_dir_all(root.join("tree/inner")).unwrap();
    let store = TempStore::new();
    store.keep(&root.join("tree/inner"), RuleScope::Directory);
    let mut parent = plan("logs", &root, "tree");
    parent.covers = vec![plan("logs", &root, "tree/inner").item];
    let trash = FakeTrash::default();
    let report = run_with(
        &store,
        &[parent],
        logs(),
        &MockChecker::none(),
        &trash,
        None,
    );
    assert!(matches!(
        outcomes(&report)[..],
        [ItemOutcome::Skipped(SkipReason::HoldsKept { .. })]
    ));
    assert!(moved(&trash).is_empty());
    let _ = remove_recursive_safe(&root);
}

#[test]
fn tinys_own_storage_is_protected_without_claiming_the_user_kept_it() {
    let store = TempStore::new();
    let parent = store.root.parent().unwrap().to_path_buf();
    let name = store
        .root
        .file_name()
        .unwrap()
        .to_string_lossy()
        .into_owned();
    let item = plan("logs", &parent, &name);
    let trash = FakeTrash::default();
    let report = run_with(&store, &[item], logs(), &MockChecker::none(), &trash, None);
    let outcome = outcomes(&report).remove(0);
    assert!(
        matches!(&outcome, ItemOutcome::Skipped(SkipReason::ProtectedPath(why)) if why.contains("Tiny")),
        "{outcome:?}"
    );
    assert!(INTERNAL_ID_PREFIX.ends_with(':'));
    assert!(moved(&trash).is_empty());
}

#[test]
fn a_path_whose_protection_cannot_be_decided_is_not_moved() {
    let root = fixture_root("uncertain", &["a", "b"]);
    let store = TempStore::new();
    store.keep(&root.join("c"), RuleScope::File);
    let items = [plan("logs", &root, "a"), plan("logs", &root, "b")];
    // Searchable but not listable: the matcher cannot tell `a` from `c`.
    fs::set_permissions(&root, fs::Permissions::from_mode(0o300)).unwrap();
    let trash = FakeTrash::default();
    let report = run_with(&store, &items, logs(), &MockChecker::none(), &trash, None);
    fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    for outcome in outcomes(&report) {
        assert!(
            matches!(&outcome, ItemOutcome::Skipped(SkipReason::KeepUncertain(why)) if why.contains("could not read")),
            "{outcome:?}"
        );
    }
    assert!(moved(&trash).is_empty());
    let _ = remove_recursive_safe(&root);
}

#[test]
fn rules_changed_between_two_moves_stop_the_second() {
    for bump_revision in [true, false] {
        let (root, items) = three("changed");
        let store = TempStore::new();
        store.keep(&root.join("elsewhere"), RuleScope::File);
        let b = root.join("b");
        let trash = HookedTrash {
            inner: FakeTrash::default(),
            after_move: Box::new(|| {
                store.rewrite(|doc| {
                    doc.revision += u64::from(bump_revision);
                    doc.next_id += 1;
                    doc.rules.push(ProtectionRule {
                        id: "r2".into(),
                        path: b.clone(),
                        scope: RuleScope::File,
                    });
                })
            }),
        };
        let report = run_with(&store, &items, logs(), &MockChecker::none(), &trash, None);
        assert_eq!(report.stopped, Some(StopReason::ProtectionChanged));
        assert_eq!(
            outcomes(&report),
            vec![
                ItemOutcome::MovedToTrash,
                ItemOutcome::NotAttempted,
                ItemOutcome::NotAttempted
            ],
            "bump_revision: {bump_revision}"
        );
        assert_eq!(moved(&trash.inner), [root.join("a")]);
        assert!(root.join("b").exists() && root.join("c").exists());
        let _ = remove_recursive_safe(&root);
    }
}

#[test]
fn unreadable_rules_between_two_moves_stop_the_run() {
    let (root, items) = three("unreadable");
    let store = TempStore::new();
    store.keep(&root.join("elsewhere"), RuleScope::File);
    let trash = HookedTrash {
        inner: FakeTrash::default(),
        after_move: Box::new(|| fs::write(store.rules_file(), b"{ not json").unwrap()),
    };
    let report = run_with(&store, &items, logs(), &MockChecker::none(), &trash, None);
    assert!(
        matches!(&report.stopped, Some(StopReason::ProtectionUnavailable(why)) if why.contains("malformed")),
        "{:?}",
        report.stopped
    );
    assert_eq!(
        outcomes(&report),
        vec![
            ItemOutcome::MovedToTrash,
            ItemOutcome::NotAttempted,
            ItemOutcome::NotAttempted
        ]
    );
    assert_eq!(moved(&trash.inner), [root.join("a")]);
    let _ = remove_recursive_safe(&root);
}

#[test]
fn a_store_that_vanishes_or_appears_mid_run_stops_it() {
    // The saved rules disappear after the first move.
    let (root, items) = three("vanish");
    let store = TempStore::new();
    store.keep(&root.join("elsewhere"), RuleScope::File);
    let trash = HookedTrash {
        inner: FakeTrash::default(),
        after_move: Box::new(|| fs::remove_dir_all(store.store.dir()).unwrap()),
    };
    let report = run_with(&store, &items, logs(), &MockChecker::none(), &trash, None);
    assert_eq!(report.stopped, Some(StopReason::ProtectionChanged));
    assert_eq!(moved(&trash.inner), [root.join("a")]);
    let _ = remove_recursive_safe(&root);

    // A first save by another session lands while this run saw no store.
    let (root, items) = three("appear");
    let store = TempStore::new();
    let other = TempStore::new();
    other.keep(&root.join("b"), RuleScope::File);
    let trash = HookedTrash {
        inner: FakeTrash::default(),
        after_move: Box::new(|| fs::rename(other.store.dir(), store.store.dir()).unwrap()),
    };
    let report = run_with(&store, &items, logs(), &MockChecker::none(), &trash, None);
    assert_eq!(report.stopped, Some(StopReason::ProtectionChanged));
    assert_eq!(moved(&trash.inner), [root.join("a")]);
    assert!(root.join("b").exists());
    let _ = remove_recursive_safe(&root);
}

#[test]
fn rules_edited_during_the_slow_checks_stop_before_the_move() {
    let root = fixture_root("slow", &["a"]);
    let store = TempStore::new();
    store.keep(&root.join("elsewhere"), RuleScope::File);
    let mut gated = fixture("logs");
    gated.app = Some("Xcode");
    let probe = HookedProbe(Box::new(|| fs::write(store.rules_file(), b"[]").unwrap()));
    let trash = FakeTrash::default();
    let report = run_with(
        &store,
        &[plan("logs", &root, "a")],
        vec![Box::new(gated)],
        &probe,
        &trash,
        None,
    );
    assert!(matches!(
        report.stopped,
        Some(StopReason::ProtectionUnavailable(_))
    ));
    assert_eq!(outcomes(&report), vec![ItemOutcome::NotAttempted]);
    assert!(moved(&trash).is_empty());
    let _ = remove_recursive_safe(&root);
}

#[test]
fn cancellation_after_the_policy_checks_still_stops_before_the_move() {
    let (root, items) = three("cancel-late");
    let store = TempStore::new();
    store.keep(&root.join("elsewhere"), RuleScope::File);
    let flag = AtomicBool::new(false);
    let mut gated = fixture("logs");
    gated.app = Some("Xcode");
    let probe = HookedProbe(Box::new(|| flag.store(true, Ordering::Release)));
    let trash = FakeTrash::default();
    let report = run_with(
        &store,
        &items,
        vec![Box::new(gated)],
        &probe,
        &trash,
        Some(&flag),
    );
    assert_eq!(report.stopped, Some(StopReason::Cancelled));
    assert_eq!(outcomes(&report), vec![ItemOutcome::NotAttempted; 3]);
    assert!(moved(&trash).is_empty());
    let _ = remove_recursive_safe(&root);
}

#[test]
fn a_busy_store_cannot_start_a_run() {
    let store = TempStore::new();
    let _holder = store.store.lease().unwrap();
    assert!(matches!(
        store.store.lease(),
        Err(crate::clean::protection_store::StoreError::Busy)
    ));
}
