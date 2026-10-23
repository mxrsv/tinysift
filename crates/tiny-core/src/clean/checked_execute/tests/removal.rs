//! Permanent deletion and the Trash row: each item is removed only the way
//! its plan says, every guard still applies, and no failure is retried
//! another way. The sinks record calls; only deletion as administrator
//! removes files, and only the ones these tests created.

use super::*;
use crate::clean::fs_safe::dir_size_checked;

/// Records every Trash move, Empty Trash, deletion and batch deleted as
/// administrator in one log, so the order across routes is visible; fails
/// the paths it is told to.
#[derive(Default)]
pub(super) struct Sinks {
    /// Shared, so a fake tool can log into the same sequence.
    pub(super) log: Arc<Mutex<Vec<String>>>,
    pub(super) fail_trash: Option<TrashError>,
    /// Fails only Empty Trash; moves still work.
    pub(super) fail_empty: Option<TrashError>,
    /// Fails with "permission denied", as a root-owned path would.
    pub(super) fail_delete: Option<&'static str>,
    pub(super) fail_elevate: Option<ElevatedError>,
    /// Left in place by an elevated batch that otherwise deletes.
    pub(super) keep_on_elevate: Option<&'static str>,
}

impl Sinks {
    pub(super) fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    fn record(&self, entry: String) {
        self.log.lock().unwrap().push(entry);
    }
}

pub(super) fn name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

impl Trash for Sinks {
    fn move_to_trash(&self, path: &Path) -> std::result::Result<(), TrashError> {
        if let Some(error) = &self.fail_trash {
            return Err(error.clone());
        }
        self.record(format!("trash {}", name(path)));
        Ok(())
    }

    fn empty_trash(&self) -> std::result::Result<(), TrashError> {
        if let Some(error) = self.fail_empty.as_ref().or(self.fail_trash.as_ref()) {
            return Err(error.clone());
        }
        self.record("empty".into());
        Ok(())
    }
}

impl Delete for Sinks {
    fn delete(&self, path: &Path) -> io::Result<()> {
        if self.fail_delete.is_some_and(|fail| path.ends_with(fail)) {
            let denied = io::Error::new(io::ErrorKind::PermissionDenied, "permission denied");
            return Err(denied);
        }
        self.record(format!("delete {}", name(path)));
        Ok(())
    }
}

/// The run trusts only what is left on disk, so unless the dialog was
/// cancelled or a simulator tool runs, this removes the batch (the tests'
/// own files) except `keep_on_elevate`, then answers `fail_elevate`.
impl ElevatedDelete for Sinks {
    fn delete(&self, paths: &[&Path]) -> std::result::Result<(), ElevatedError> {
        let names: Vec<String> = paths.iter().map(|path| name(path)).collect();
        self.record(format!("elevate {}", names.join(" ")));
        let ran = !matches!(
            self.fail_elevate,
            Some(ElevatedError::Cancelled | ElevatedError::Busy(_))
        );
        let kept = |path: &&&Path| {
            self.keep_on_elevate
                .is_some_and(|keep| path.ends_with(keep))
        };
        for path in paths.iter().filter(|path| ran && !kept(path)) {
            remove_recursive_safe(path).unwrap();
        }
        self.fail_elevate.clone().map_or(Ok(()), Err)
    }
}

fn run_sinks(
    store: &TempStore,
    plan: &[PlannedItem],
    probe: &dyn AppProbe,
    sinks: &Sinks,
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
        cancel: None,
        progress: None,
        policy: &lease,
    };
    execute_checked(plan, &ctx)
}

/// Registered ids, so the sections are the real ones: Rebuilt, build
/// output, Your files, and the Trash row.
fn providers() -> Vec<Box<dyn CleanProvider>> {
    let mut rust = fixture("rust-targets");
    rust.guard = Some("guarded");
    rust.app = Some("cargo");
    let trash = Fixture {
        desktop: false,
        risk: RiskLevel::Destructive,
        empties_trash: true,
        ..fixture("trash")
    };
    vec![
        Box::new(fixture("user-logs")),
        Box::new(rust),
        Box::new(fixture("downloads-old")),
        Box::new(trash),
    ]
}

pub(super) fn planned(category: &str, root: &Path, name: &str, removal: Removal) -> PlannedItem {
    PlannedItem {
        removal,
        ..plan(category, root, name)
    }
}

/// `root/.Trash` holding one file, planned as the Trash row.
pub(super) fn trash_row(root: &Path) -> PlannedItem {
    let bin = root.join(".Trash");
    fs::create_dir(&bin).unwrap();
    fs::write(bin.join("old"), b"1234").unwrap();
    // A fixed past time, so the next change shows on any clock granularity.
    let past = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_000_000);
    fs::File::open(&bin).unwrap().set_modified(past).unwrap();
    let meta = fs::symlink_metadata(&bin).unwrap();
    PlannedItem {
        fingerprint: Some(PathFingerprint::of_listing(&meta)),
        roots: vec![bin.clone()],
        ..planned("trash", root, ".Trash", Removal::EmptyTrash)
    }
}

#[test]
fn permanent_items_are_deleted_and_your_files_still_go_to_the_trash() {
    let root = fixture_root("delete", &["a", "b", "c"]);
    let plan = [
        planned("user-logs", &root, "a", Removal::Delete),
        planned("rust-targets", &root, "b", Removal::Delete),
        planned("downloads-old", &root, "c", Removal::MoveToTrash),
    ];
    let sinks = Sinks::default();
    let report = run_sinks(&TempStore::new(), &plan, &MockChecker::none(), &sinks);
    assert_eq!(
        outcomes(&report),
        vec![
            ItemOutcome::Deleted,
            ItemOutcome::Deleted,
            ItemOutcome::MovedToTrash
        ]
    );
    assert_eq!(sinks.log(), ["delete a", "delete b", "trash c"]);
    assert_eq!(report.bytes_freed, 8, "deleted bytes are freed");
    assert_eq!(report.bytes_moved_to_trash(), 4, "moved bytes are not");
    assert!(!report.freed_is_upper_bound);
    assert_eq!(report.count(ItemOutcome::frees_space), 2);
    let _ = remove_recursive_safe(&root);
}

#[test]
fn permanent_removals_run_before_any_move_to_the_trash() {
    let root = fixture_root("order", &["a", "c"]);
    let row = trash_row(&root);
    let plan = [
        planned("downloads-old", &root, "c", Removal::MoveToTrash),
        row,
        planned("user-logs", &root, "a", Removal::Delete),
    ];
    let sinks = Sinks::default();
    let report = run_sinks(&TempStore::new(), &plan, &MockChecker::none(), &sinks);
    assert_eq!(
        outcomes(&report),
        vec![
            ItemOutcome::MovedToTrash,
            ItemOutcome::TrashEmptied,
            ItemOutcome::Deleted
        ],
        "results stay in plan order"
    );
    assert_eq!(
        sinks.log(),
        ["delete a", "empty", "trash c"],
        "emptying the Trash never takes what this run moved there"
    );
    assert_eq!(report.bytes_freed, 8, "the deleted item and the Trash row");
    let _ = remove_recursive_safe(&root);
}

#[test]
fn a_failed_emptying_moves_nothing_into_the_trash_while_other_removals_go_on() {
    let root = fixture_root("empty-fails", &["a", "c"]);
    let plan = [
        planned("downloads-old", &root, "c", Removal::MoveToTrash),
        trash_row(&root),
        planned("user-logs", &root, "a", Removal::Delete),
    ];
    let sinks = Sinks {
        fail_empty: Some(TrashError::Failed("-1712 timed out".into())),
        ..Default::default()
    };
    let report = run_sinks(&TempStore::new(), &plan, &MockChecker::none(), &sinks);
    assert_eq!(
        outcomes(&report),
        vec![
            ItemOutcome::Skipped(SkipReason::SafetyCheckFailed(
                "the Trash was not emptied cleanly, so nothing is moved into it".into()
            )),
            ItemOutcome::Failed {
                error: TrashError::Failed("-1712 timed out".into()),
                source_present: true,
            },
            ItemOutcome::Deleted,
        ]
    );
    assert_eq!(
        sinks.log(),
        ["delete a"],
        "Finder may still be emptying: nothing goes into the Trash after it"
    );
    assert_eq!(report.bytes_freed, 4, "only the deleted item");
    assert_eq!(
        report.stopped, None,
        "a failure other than a denial goes on"
    );
    assert!(root.join("c").exists());
    let _ = remove_recursive_safe(&root);
}

#[test]
fn an_emptying_that_never_ran_leaves_the_moves_alone() {
    let root = fixture_root("empty-skipped", &["c"]);
    let mut row = trash_row(&root);
    // Changed since the listing: skipped before Finder is asked.
    fs::write(root.join(".Trash/new"), b"added").unwrap();
    row.fingerprint = None;
    let plan = [
        planned("downloads-old", &root, "c", Removal::MoveToTrash),
        row,
    ];
    let sinks = Sinks::default();
    let report = run_sinks(&TempStore::new(), &plan, &MockChecker::none(), &sinks);
    assert_eq!(
        outcomes(&report),
        vec![
            ItemOutcome::MovedToTrash,
            ItemOutcome::Skipped(SkipReason::Changed)
        ]
    );
    assert_eq!(sinks.log(), ["trash c"]);
    let _ = remove_recursive_safe(&root);
}

#[test]
fn a_failed_trash_move_is_never_retried_as_a_deletion() {
    let root = fixture_root("trash-fails", &["a", "c"]);
    let plan = [
        planned("user-logs", &root, "a", Removal::MoveToTrash),
        planned("downloads-old", &root, "c", Removal::MoveToTrash),
    ];
    let sinks = Sinks {
        fail_trash: Some(TrashError::Failed("Finder error".into())),
        ..Default::default()
    };
    let report = run_sinks(&TempStore::new(), &plan, &MockChecker::none(), &sinks);
    let failed = ItemOutcome::Failed {
        error: TrashError::Failed("Finder error".into()),
        source_present: true,
    };
    assert_eq!(outcomes(&report), vec![failed.clone(), failed]);
    assert!(sinks.log().is_empty(), "nothing deleted: {:?}", sinks.log());
    assert_eq!(report.bytes_freed, 0);
    assert!(root.join("a").exists() && root.join("c").exists());
    let _ = remove_recursive_safe(&root);
}

#[test]
fn a_failed_deletion_is_never_moved_to_the_trash_or_retried() {
    let root = fixture_root("delete-fails", &["a", "b"]);
    let plan = [
        planned("user-logs", &root, "a", Removal::Delete),
        planned("rust-targets", &root, "b", Removal::Delete),
    ];
    let sinks = Sinks {
        fail_delete: Some("a"),
        ..Default::default()
    };
    let report = run_sinks(&TempStore::new(), &plan, &MockChecker::none(), &sinks);
    assert_eq!(
        outcomes(&report),
        vec![
            ItemOutcome::DeleteFailed {
                detail: "permission denied".into(),
                source_present: true,
            },
            ItemOutcome::Deleted,
        ],
        "each item reports its own outcome and the run goes on"
    );
    assert_eq!(
        sinks.log(),
        ["delete b"],
        "no Trash, no administrator rights, no second attempt"
    );
    assert_eq!(report.stopped, None);
    assert_eq!(report.bytes_freed, 4, "only what was deleted");
    let _ = remove_recursive_safe(&root);
}

#[test]
fn a_denied_empty_trash_stops_the_run_like_a_denied_move() {
    let root = fixture_root("empty-denied", &["c"]);
    let plan = [
        trash_row(&root),
        planned("downloads-old", &root, "c", Removal::MoveToTrash),
    ];
    let sinks = Sinks {
        fail_trash: Some(TrashError::AutomationDenied("(-1743)".into())),
        ..Default::default()
    };
    let report = run_sinks(&TempStore::new(), &plan, &MockChecker::none(), &sinks);
    assert_eq!(report.stopped, Some(StopReason::AutomationDenied));
    assert!(matches!(outcomes(&report)[0], ItemOutcome::Failed { .. }));
    assert_eq!(outcomes(&report)[1], ItemOutcome::NotAttempted);
    assert!(sinks.log().is_empty());
    let _ = remove_recursive_safe(&root);
}

#[test]
fn the_executor_refuses_a_removal_the_category_does_not_allow() {
    let root = fixture_root("mismatch", &["a", "b", "c"]);
    let row = trash_row(&root);
    let your_file = planned("downloads-old", &root, "c", Removal::Delete);
    let carrying = PlannedItem {
        covers: vec![plan("downloads-old", &root, "c").item],
        ..planned("user-logs", &root, "a", Removal::Delete)
    };
    let not_trash = planned("user-logs", &root, "b", Removal::EmptyTrash);
    let row_moved = PlannedItem {
        removal: Removal::MoveToTrash,
        ..row.clone()
    };
    let row_deleted = PlannedItem {
        removal: Removal::Delete,
        ..row
    };
    let sinks = Sinks::default();
    let report = run_sinks(
        &TempStore::new(),
        &[your_file, carrying, not_trash, row_moved, row_deleted],
        &MockChecker::none(),
        &sinks,
    );
    let only_trash = || {
        ItemOutcome::Skipped(SkipReason::ProviderGuard(
            "only the Trash may take it".into(),
        ))
    };
    assert_eq!(
        outcomes(&report),
        vec![
            only_trash(),
            only_trash(),
            ItemOutcome::Skipped(SkipReason::ReportOnly),
            ItemOutcome::Skipped(SkipReason::ReportOnly),
            ItemOutcome::Skipped(SkipReason::ReportOnly),
        ]
    );
    assert!(sinks.log().is_empty(), "{:?}", sinks.log());
    let _ = remove_recursive_safe(&root);
}

#[test]
fn permanent_deletion_keeps_every_guard() {
    let root = fixture_root("guards", &["kept", "guarded", "running", "changed"]);
    fs::create_dir(root.join("real")).unwrap();
    std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
    let store = TempStore::new();
    store.keep(&root.join("kept"), RuleScope::File);
    let row = trash_row(&root);
    store.keep(&root.join(".Trash/old"), RuleScope::File);
    let changed = planned("user-logs", &root, "changed", Removal::Delete);
    fs::write(root.join("changed"), b"edited since the scan").unwrap();
    let mut system = planned("user-logs", &root, "kept", Removal::Delete);
    system.item.path = PathBuf::from("/System/Library/tiny-test-missing");
    let running = planned("rust-targets", &root, "running", Removal::Delete);
    let plan = [
        planned("user-logs", &root, "kept", Removal::Delete),
        planned("rust-targets", &root, "guarded", Removal::Delete),
        PlannedItem {
            fingerprint: fingerprint(&root.join("link")).ok(),
            ..planned("user-logs", &root, "link", Removal::Delete)
        },
        changed,
        system,
        row,
    ];
    let sinks = Sinks::default();
    let report = run_sinks(&store, &plan, &MockChecker::none(), &sinks);
    let skipped = outcomes(&report);
    assert!(matches!(
        skipped[0],
        ItemOutcome::Skipped(SkipReason::Kept { .. })
    ));
    assert_eq!(
        skipped[1],
        ItemOutcome::Skipped(SkipReason::ProviderGuard("guarded is protected".into()))
    );
    assert_eq!(skipped[2], ItemOutcome::Skipped(SkipReason::Symlink));
    assert_eq!(skipped[3], ItemOutcome::Skipped(SkipReason::Changed));
    assert_eq!(
        skipped[4],
        ItemOutcome::Skipped(SkipReason::ProtectedPath("inside /System".into()))
    );
    assert!(
        matches!(
            skipped[5],
            ItemOutcome::Skipped(SkipReason::HoldsKept { .. })
        ),
        "a kept path inside the Trash keeps it from being emptied"
    );

    let report = run_sinks(
        &TempStore::new(),
        &[running],
        &MockChecker::with_running(["cargo"]),
        &sinks,
    );
    assert_eq!(
        outcomes(&report),
        vec![ItemOutcome::Skipped(SkipReason::AppRunning("cargo".into()))]
    );
    assert!(sinks.log().is_empty(), "{:?}", sinks.log());
    let _ = remove_recursive_safe(&root);
}

#[test]
fn the_trash_is_emptied_only_as_discovery_listed_it() {
    let root = fixture_root("trash-changed", &[]);
    let row = trash_row(&root);
    fs::write(root.join(".Trash/added later"), b"not reviewed").unwrap();
    let sinks = Sinks::default();
    let report = run_sinks(&TempStore::new(), &[row], &MockChecker::none(), &sinks);
    assert_eq!(
        outcomes(&report),
        vec![ItemOutcome::Skipped(SkipReason::Changed)]
    );
    assert!(sinks.log().is_empty());
    let _ = remove_recursive_safe(&root);
}

/// `name` under `root` with the size and shared blocks discovery measures.
fn measured(category: &str, root: &Path, name: &str) -> PlannedItem {
    let path = root.join(name);
    let ctx = ScanContext::unchecked();
    let size = dir_size_checked(&path, &ctx);
    let mut findings = ctx.take_findings();
    let mut planned = planned(category, root, name, Removal::Delete);
    planned.item.size = size;
    planned.size_is_upper_bound = findings.size_upper_bound.contains(&path);
    planned.shared = findings.shared.remove(&path).unwrap_or_default();
    planned
}

#[test]
fn a_hard_link_split_between_deleted_items_is_freed_once_both_are_gone() {
    let root = fixture_root("split", &[]);
    for dir in ["a", "b"] {
        fs::create_dir(root.join(dir)).unwrap();
    }
    fs::write(root.join("a/own"), vec![1u8; 20_000]).unwrap();
    fs::write(root.join("a/linked"), vec![2u8; 50_000]).unwrap();
    fs::hard_link(root.join("a/linked"), root.join("b/linked")).unwrap();
    let linked = fs::symlink_metadata(root.join("a/linked"))
        .unwrap()
        .blocks()
        * 512;
    let plan = [
        measured("user-logs", &root, "a"),
        measured("rust-targets", &root, "b"),
    ];
    let own = plan[0].item.size;
    assert_eq!(plan[1].item.size, 0, "each item alone leaves the inode out");

    let report = run_sinks(
        &TempStore::new(),
        &plan,
        &MockChecker::none(),
        &Sinks::default(),
    );
    assert_eq!(report.bytes_freed, own + linked, "counted once");
    // Only macOS can tell which blocks other files share.
    assert_eq!(report.freed_is_upper_bound, cfg!(not(target_os = "macos")));

    let one_left = Sinks {
        fail_delete: Some("b"),
        ..Default::default()
    };
    let report = run_sinks(&TempStore::new(), &plan, &MockChecker::none(), &one_left);
    assert_eq!(report.bytes_freed, own, "the link left in b keeps the data");
    let _ = remove_recursive_safe(&root);
}
