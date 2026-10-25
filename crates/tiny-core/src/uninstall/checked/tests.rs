//! Checked uninstall against temp directories, a fake `Trash`, fake probes
//! and a temp protection store. Nothing here touches Finder or real data.

use std::os::unix::fs::symlink;
use std::sync::atomic::AtomicUsize;
use std::sync::Mutex;

use tempfile::TempDir;

use super::*;
use crate::clean::process::test_support::{FailingProbe, MockChecker};
use crate::clean::protection::{ProtectionRule, RuleScope, RulesDocument};
use crate::clean::protection_store::ProtectionStore;
use crate::uninstall::{AppEntry, LeftoverClass, RemovalItem};

const ID: &str = "com.example.App";
const EXE: &str = "AppExec";

const BUNDLE: usize = 0;
const CACHE: usize = 1;
const DATA: usize = 2;
const GROUP: usize = 3;
const ODD: usize = 4;

/// Records each move. `before` runs first (to swap the filesystem under the
/// mover), `after` once the move is recorded.
struct TestTrash<'a> {
    moved: Mutex<Vec<PathBuf>>,
    fail_on: Option<(&'static str, TrashError)>,
    before: Box<dyn Fn(&Path) + Send + Sync + 'a>,
    after: Box<dyn Fn(&Path) + Send + Sync + 'a>,
}

impl TestTrash<'_> {
    fn new() -> Self {
        Self {
            moved: Mutex::default(),
            fail_on: None,
            before: Box::new(|_| {}),
            after: Box::new(|_| {}),
        }
    }
    fn failing(name: &'static str, error: TrashError) -> Self {
        Self {
            fail_on: Some((name, error)),
            ..Self::new()
        }
    }
    fn moved(&self) -> Vec<PathBuf> {
        self.moved.lock().unwrap().clone()
    }
}

impl Trash for TestTrash<'_> {
    fn move_to_trash(&self, path: &Path) -> Result<(), TrashError> {
        (self.before)(path);
        if let Some((name, error)) = &self.fail_on {
            if path.ends_with(name) {
                return Err(error.clone());
            }
        }
        self.moved.lock().unwrap().push(path.to_path_buf());
        (self.after)(path);
        Ok(())
    }
}

/// Answers "not running" until its `running_from`-th call, then "running".
/// Call 0 is the run's own up-front check.
struct ScriptedProbe {
    calls: AtomicUsize,
    running_from: usize,
    on_call: Box<dyn Fn(usize) + Send + Sync>,
}

impl ScriptedProbe {
    fn new(running_from: usize) -> Self {
        Self {
            calls: AtomicUsize::new(0),
            running_from,
            on_call: Box::new(|_| {}),
        }
    }
}

impl AppProbe for ScriptedProbe {
    fn probe(&self, name: &str) -> Result<bool, String> {
        assert_eq!(name, EXE);
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        (self.on_call)(call);
        Ok(call >= self.running_from)
    }
}

struct World {
    _root: TempDir,
    _store_root: TempDir,
    home: PathBuf,
    apps: PathBuf,
    store: ProtectionStore,
    plan: CheckedPlan,
}

fn real_tempdir() -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    // Physical path, not macOS's /var temp alias.
    let path = fs::canonicalize(dir.path()).unwrap();
    (dir, path)
}

fn make_dir(path: &Path) {
    fs::create_dir_all(path.join("sub")).unwrap();
    fs::write(path.join("sub/f"), b"1234").unwrap();
}

impl World {
    fn new() -> Self {
        Self::with_executable(Some(EXE))
    }

    fn with_executable(exe: Option<&str>) -> Self {
        Self::build(exe, &[])
    }

    /// Like `new`, but the listed leftovers vanish before the plan is frozen.
    fn build(exe: Option<&str>, vanish: &[usize]) -> Self {
        let (root, base) = real_tempdir();
        let (store_root, store_base) = real_tempdir();
        let (home, apps) = (base.join("home"), base.join("apps"));
        let lib = home.join("Library");
        let paths = [
            apps.join("App.app"),
            lib.join("Caches").join(ID),
            lib.join("Application Support").join(ID),
            lib.join("Group Containers").join(format!("group.{ID}")),
            lib.join("Containers").join(ID),
        ];
        paths.iter().for_each(|p| make_dir(p));
        let items_gone: Vec<_> = vanish.iter().map(|&i| paths[i].clone()).collect();
        let owned = |class| Ownership::Owned { class };
        let reason = |r: &str| r.to_string();
        let ownerships = [
            Ownership::AppBundle,
            owned(LeftoverClass::Regenerable),
            owned(LeftoverClass::DataRisk),
            Ownership::ReportOnly {
                reason: reason("shared"),
            },
            Ownership::Uncertain {
                reason: reason("odd"),
            },
        ];
        let items = paths
            .into_iter()
            .zip(ownerships)
            .enumerate()
            .map(|(i, (path, ownership))| RemovalItem {
                path,
                size: 4,
                kind: if i == BUNDLE {
                    ItemKind::AppBundle
                } else {
                    ItemKind::Leftover
                },
                ownership,
            })
            .collect();
        let plan = Plan {
            app: AppEntry {
                name: "App".into(),
                path: apps.join("App.app"),
                bundle_id: Some(ID.into()),
                size: 4,
                last_used_days: None,
            },
            items,
            blocked: None,
        };
        items_gone
            .iter()
            .for_each(|p| fs::remove_dir_all(p).unwrap());
        let plan = build_checked_plan(&plan, &home, &apps, exe.map(String::from)).unwrap();
        let store = ProtectionStore::at(store_base.join("clean-protection"));
        Self {
            _root: root,
            _store_root: store_root,
            home,
            apps,
            store,
            plan,
        }
    }

    fn path(&self, index: usize) -> PathBuf {
        self.plan.items[index].path.clone()
    }

    fn run_with(
        &self,
        select: &[usize],
        probe: &dyn AppProbe,
        trash: &dyn Trash,
        cancel: Option<&AtomicBool>,
    ) -> Result<UninstallReport, UninstallRefusal> {
        let lease = self.store.lease().unwrap();
        let ctx = UninstallContext {
            home: &self.home,
            probe,
            trash,
            cancel,
            progress: None,
            policy: &lease,
        };
        execute_checked_uninstall(&self.plan, &select.iter().copied().collect(), &ctx)
    }

    fn run(&self, select: &[usize], trash: &dyn Trash) -> UninstallReport {
        self.run_with(select, &MockChecker::none(), trash, None)
            .unwrap()
    }

    fn keep(&self, path: &Path, scope: RuleScope) {
        let mut lease = self.store.lease().unwrap();
        let revision = lease.snapshot().revision;
        lease.add(revision, path, scope).unwrap();
    }

    /// Rewrites the rules as another process would: atomically, owner-only.
    fn rewrite(&self, edit: impl FnOnce(&mut RulesDocument)) {
        use std::os::unix::fs::PermissionsExt;
        let file = self.store.dir().join("rules.json");
        let mut doc: RulesDocument = serde_json::from_slice(&fs::read(&file).unwrap()).unwrap();
        edit(&mut doc);
        let staged = self.store.dir().with_file_name("rules.next");
        fs::write(&staged, serde_json::to_vec(&doc).unwrap()).unwrap();
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o600)).unwrap();
        fs::rename(&staged, &file).unwrap();
    }
}

fn outcomes(report: &UninstallReport) -> Vec<ItemOutcome> {
    report.results.iter().map(|r| r.outcome.clone()).collect()
}

fn skipped(reason: SkipReason) -> ItemOutcome {
    ItemOutcome::Skipped(reason)
}

/// Swaps `dir` for a symlink to its renamed original.
fn swap_for_symlink(dir: &Path) {
    let real = dir.with_file_name(format!(
        "{}-real",
        dir.file_name().unwrap().to_string_lossy()
    ));
    fs::rename(dir, &real).unwrap();
    symlink(&real, dir).unwrap();
}
#[test]
fn explicit_selection_moves_bundle_first_then_leftovers() {
    let world = World::new();
    let trash = TestTrash::new();
    let report = world.run(&[DATA, CACHE, BUNDLE], &trash);
    assert_eq!(outcomes(&report), vec![ItemOutcome::MovedToTrash; 3]);
    assert_eq!(report.stopped, None);
    assert_eq!(
        trash.moved(),
        [world.path(BUNDLE), world.path(CACHE), world.path(DATA)]
    );
    assert_eq!(
        (report.moved_count(), report.bytes_moved_to_trash()),
        (3, 12)
    );
}
#[test]
fn unselected_items_are_never_moved() {
    let world = World::new();
    let trash = TestTrash::new();
    let report = world.run(&[BUNDLE, CACHE], &trash);
    assert_eq!(report.results.len(), 2);
    assert_eq!(trash.moved(), [world.path(BUNDLE), world.path(CACHE)]);
    // Leftovers alone work too: the bundle is not required.
    let trash = TestTrash::new();
    world.run(&[DATA], &trash);
    assert_eq!(trash.moved(), [world.path(DATA)]);
}
#[test]
fn unselectable_or_unknown_selections_refuse_the_whole_run() {
    let world = World::new();
    let cases: [(&[usize], UninstallRefusal); 4] = [
        (
            &[CACHE, GROUP],
            UninstallRefusal::NotSelectable {
                index: GROUP,
                reason: "shared".into(),
            },
        ),
        (
            &[ODD],
            UninstallRefusal::NotSelectable {
                index: ODD,
                reason: "odd".into(),
            },
        ),
        (&[BUNDLE, 9], UninstallRefusal::UnknownItem(9)),
        (&[], UninstallRefusal::NothingSelected),
    ];
    for (select, expected) in cases {
        let trash = TestTrash::new();
        let result = world.run_with(select, &MockChecker::none(), &trash, None);
        assert_eq!(result.unwrap_err(), expected);
        assert!(trash.moved().is_empty());
    }
}
#[test]
fn a_running_or_unknowable_app_moves_nothing() {
    let world = World::new();
    let trash = TestTrash::new();
    let running = MockChecker::with_running([EXE]);
    let result = world.run_with(&[BUNDLE, CACHE], &running, &trash, None);
    assert_eq!(
        result.unwrap_err(),
        UninstallRefusal::AppRunning(EXE.into())
    );
    let result = world.run_with(&[BUNDLE], &FailingProbe, &trash, None);
    assert!(matches!(
        result,
        Err(UninstallRefusal::AppCheckFailed { .. })
    ));
    for exe in [None, Some("  "), Some("a/b")] {
        let world = World::with_executable(exe);
        let result = world.run_with(&[BUNDLE], &MockChecker::none(), &trash, None);
        assert_eq!(
            result.unwrap_err(),
            UninstallRefusal::ExecutableUnknown,
            "{exe:?}"
        );
    }
    assert!(trash.moved().is_empty());
}
#[test]
fn an_app_that_starts_mid_run_stops_the_run() {
    let world = World::new();
    let trash = TestTrash::new();
    // Call 0 is the up-front check, 1 the bundle's, 2 the cache's.
    let report = world.run_with(&[BUNDLE, CACHE, DATA], &ScriptedProbe::new(2), &trash, None);
    let report = report.unwrap();
    assert_eq!(report.stopped, Some(StopReason::AppRunning(EXE.into())));
    assert_eq!(
        outcomes(&report),
        [
            ItemOutcome::MovedToTrash,
            ItemOutcome::NotAttempted,
            ItemOutcome::NotAttempted
        ]
    );
}
#[test]
fn a_leftover_that_changed_is_skipped_and_the_rest_proceed() {
    let world = World::new();
    let cache = world.path(CACHE);
    fs::rename(&cache, cache.with_extension("old")).unwrap();
    make_dir(&cache); // a different directory (inode) at the same path
    let trash = TestTrash::new();
    let report = world.run(&[BUNDLE, CACHE, DATA], &trash);
    assert_eq!(
        outcomes(&report),
        [
            ItemOutcome::MovedToTrash,
            skipped(SkipReason::Changed),
            ItemOutcome::MovedToTrash
        ]
    );
    assert_eq!(trash.moved(), [world.path(BUNDLE), world.path(DATA)]);
}
#[test]
fn symlinked_missing_and_symlinked_parent_items_are_skipped() {
    let world = World::new();
    let (cache, data) = (world.path(CACHE), world.path(DATA));
    swap_for_symlink(&cache);
    fs::remove_dir_all(&data).unwrap();
    let trash = TestTrash::new();
    let report = world.run(&[CACHE, DATA], &trash);
    assert_eq!(
        outcomes(&report),
        [skipped(SkipReason::Symlink), skipped(SkipReason::Missing)]
    );
    // A symlinked ancestor: same inode as before, so only the walk can tell.
    let world = World::new();
    swap_for_symlink(world.path(CACHE).parent().unwrap());
    let report = world.run(&[CACHE, DATA], &trash);
    assert_eq!(
        outcomes(&report),
        [skipped(SkipReason::OutsideRoots), ItemOutcome::MovedToTrash]
    );
    assert_eq!(trash.moved(), [world.path(DATA)]);
}
#[test]
fn an_item_outside_its_container_is_skipped_and_a_skipped_bundle_blocks_leftovers() {
    let mut world = World::new();
    world.plan.items[BUNDLE].container = world.home.join("elsewhere");
    let trash = TestTrash::new();
    let report = world.run(&[BUNDLE, CACHE], &trash);
    assert_eq!(
        outcomes(&report),
        [skipped(SkipReason::OutsideRoots), ItemOutcome::NotAttempted]
    );
    assert_eq!(report.stopped, None);
    // The container itself is not "inside" it.
    let mut world = World::new();
    world.plan.items[CACHE].container = world.path(CACHE);
    let report = world.run(&[CACHE], &trash);
    assert_eq!(outcomes(&report), [skipped(SkipReason::OutsideRoots)]);
    assert!(trash.moved().is_empty());
}
#[test]
fn a_protected_path_is_refused() {
    let mut world = World::new();
    world.plan.items[CACHE].container = PathBuf::from("/");
    let home = world.home.clone();
    world.plan.items[CACHE].path = home.clone();
    world.plan.items[CACHE].fingerprint =
        Ok(PathFingerprint::of(&fs::symlink_metadata(&home).unwrap()));
    let trash = TestTrash::new();
    let report = world.run(&[CACHE], &trash);
    assert_eq!(
        outcomes(&report),
        [skipped(SkipReason::ProtectedPath("the home folder".into()))]
    );
    assert!(trash.moved().is_empty());
}
#[test]
fn always_keep_rules_skip_only_what_they_hold() {
    let world = World::new();
    world.keep(&world.path(CACHE), RuleScope::Directory);
    let inner = world.path(DATA).join("sub/precious");
    world.keep(&inner, RuleScope::File);
    let trash = TestTrash::new();
    let report = world.run(&[BUNDLE, CACHE, DATA], &trash);
    assert_eq!(
        outcomes(&report),
        [
            ItemOutcome::MovedToTrash,
            skipped(SkipReason::Kept {
                rule_path: world.path(CACHE)
            }),
            skipped(SkipReason::HoldsKept { rule_path: inner }),
        ]
    );
    assert_eq!(trash.moved(), [world.path(BUNDLE)]);
}
#[test]
fn rules_changed_mid_run_stop_the_rest() {
    let world = World::new();
    world.keep(&world.home.join("unrelated"), RuleScope::File);
    let mut trash = TestTrash::new();
    trash.after = Box::new(|_| {
        world.rewrite(|doc| {
            doc.revision += 1;
            doc.next_id += 1;
            doc.rules.push(ProtectionRule {
                id: "r2".into(),
                path: world.path(CACHE),
                scope: RuleScope::File,
            });
        })
    });
    let report = world.run(&[BUNDLE, CACHE, DATA], &trash);
    assert_eq!(report.stopped, Some(StopReason::ProtectionChanged));
    assert_eq!(
        outcomes(&report),
        [
            ItemOutcome::MovedToTrash,
            ItemOutcome::NotAttempted,
            ItemOutcome::NotAttempted
        ]
    );
    assert_eq!(trash.moved(), [world.path(BUNDLE)]);
}
#[test]
fn cancellation_stops_before_the_first_item_and_mid_run() {
    let world = World::new();
    let flag = AtomicBool::new(true);
    let trash = TestTrash::new();
    let report = world
        .run_with(&[BUNDLE, CACHE], &MockChecker::none(), &trash, Some(&flag))
        .unwrap();
    assert_eq!(report.stopped, Some(StopReason::Cancelled));
    assert_eq!(outcomes(&report), vec![ItemOutcome::NotAttempted; 2]);
    assert!(trash.moved().is_empty());

    flag.store(false, Ordering::SeqCst);
    let mut trash = TestTrash::new();
    trash.after = Box::new(|_| flag.store(true, Ordering::SeqCst));
    let report = world
        .run_with(
            &[BUNDLE, CACHE, DATA],
            &MockChecker::none(),
            &trash,
            Some(&flag),
        )
        .unwrap();
    assert_eq!(report.stopped, Some(StopReason::Cancelled));
    assert_eq!(
        outcomes(&report),
        [
            ItemOutcome::MovedToTrash,
            ItemOutcome::NotAttempted,
            ItemOutcome::NotAttempted
        ]
    );
}
#[test]
fn automation_denial_stops_and_a_failed_bundle_protects_the_leftovers() {
    let denied = TrashError::AutomationDenied("no".into());
    let world = World::new();
    let trash = TestTrash::failing("Caches/com.example.App", denied.clone());
    let report = world.run(&[BUNDLE, CACHE, DATA], &trash);
    assert_eq!(report.stopped, Some(StopReason::AutomationDenied));
    assert_eq!(
        outcomes(&report),
        [
            ItemOutcome::MovedToTrash,
            ItemOutcome::Failed {
                error: denied,
                source_present: true
            },
            ItemOutcome::NotAttempted,
        ]
    );
    // An ordinary bundle failure ends the bundle's turn, not with a stop reason.
    let failed = TrashError::Failed("busy".into());
    let trash = TestTrash::failing("App.app", failed.clone());
    let report = world.run(&[BUNDLE, CACHE, DATA], &trash);
    assert_eq!(report.stopped, None);
    assert_eq!(
        outcomes(&report),
        [
            ItemOutcome::Failed {
                error: failed,
                source_present: true
            },
            ItemOutcome::NotAttempted,
            ItemOutcome::NotAttempted,
        ]
    );
    assert!(trash.moved().is_empty());
}
#[test]
fn a_swap_during_the_app_check_is_caught() {
    let world = World::new();
    let cache = world.path(CACHE);
    let mut probe = ScriptedProbe::new(usize::MAX);
    // Call 1 is the bundle's check, call 2 the cache's: swap while it runs.
    probe.on_call = Box::new(move |call| {
        if call == 2 {
            swap_for_symlink(&cache);
        }
    });
    let trash = TestTrash::new();
    let report = world
        .run_with(&[BUNDLE, CACHE], &probe, &trash, None)
        .unwrap();
    assert_eq!(
        outcomes(&report),
        [ItemOutcome::MovedToTrash, skipped(SkipReason::Symlink)]
    );
}

/// Documents the gap: the mover is path-based, so a swap after validation
/// is still handed over. This is NOT evidence of atomicity.
#[test]
fn residual_race_a_swap_inside_the_mover_is_still_handed_over() {
    let world = World::new();
    let cache = world.path(CACHE);
    let mut trash = TestTrash::new();
    let target = cache.clone();
    trash.before = Box::new(move |path| {
        if path == target {
            swap_for_symlink(&target);
        }
    });
    let report = world.run(&[CACHE], &trash);
    assert_eq!(outcomes(&report), [ItemOutcome::MovedToTrash]);
    assert_eq!(trash.moved(), vec![cache.clone()]);
    assert!(fs::symlink_metadata(&cache)
        .unwrap()
        .file_type()
        .is_symlink());
}
#[test]
fn build_refuses_blocked_inconsistent_and_unreadable_plans() {
    let world = World::new();
    let item = |path: PathBuf, kind, ownership| RemovalItem {
        path,
        size: 1,
        kind,
        ownership,
    };
    let bundle = || {
        item(
            world.path(BUNDLE),
            ItemKind::AppBundle,
            Ownership::AppBundle,
        )
    };
    let mut plan = Plan {
        app: AppEntry {
            name: "App".into(),
            path: world.path(BUNDLE),
            bundle_id: None,
            size: 0,
            last_used_days: None,
        },
        items: vec![bundle()],
        blocked: Some("system app".into()),
    };
    let build = |plan: &Plan| build_checked_plan(plan, &world.home, &world.apps, Some(EXE.into()));
    assert_eq!(
        build(&plan).unwrap_err(),
        UninstallRefusal::Blocked("system app".into())
    );
    plan.blocked = None;
    assert!(build(&plan).is_ok());
    plan.items.push(bundle());
    assert!(matches!(
        build(&plan),
        Err(UninstallRefusal::InconsistentPlan(_))
    ));
    plan.items[1] = item(world.path(CACHE), ItemKind::Leftover, Ownership::AppBundle);
    assert!(matches!(
        build(&plan),
        Err(UninstallRefusal::InconsistentPlan(_))
    ));
    plan.items.truncate(1);
    plan.items[0].path = world.apps.join("Missing.app");
    assert!(matches!(
        build(&plan),
        Err(UninstallRefusal::Unreadable { .. })
    ));
}
#[test]
fn a_vanished_leftover_keeps_every_index_in_place() {
    let world = World::build(Some(EXE), &[CACHE]);
    assert_eq!(world.plan.items().len(), 5);
    let trash = TestTrash::new();
    let report = world.run(&[CACHE, DATA], &trash);
    assert_eq!(
        outcomes(&report),
        [skipped(SkipReason::Missing), ItemOutcome::MovedToTrash]
    );
    assert_eq!(trash.moved(), [world.path(DATA)]);
}
#[test]
fn a_kept_bundle_leaves_every_leftover_untouched() {
    let world = World::new();
    world.keep(&world.path(BUNDLE), RuleScope::Directory);
    let trash = TestTrash::new();
    let report = world.run(&[BUNDLE, CACHE, DATA], &trash);
    assert_eq!(
        outcomes(&report),
        [
            skipped(SkipReason::Kept {
                rule_path: world.path(BUNDLE)
            }),
            ItemOutcome::NotAttempted,
            ItemOutcome::NotAttempted,
        ]
    );
    assert_eq!(report.stopped, None);
    assert!(trash.moved().is_empty());
}
#[test]
fn an_unreadable_store_mid_run_stops_the_rest() {
    use std::os::unix::fs::PermissionsExt;
    let world = World::new();
    world.keep(&world.home.join("unrelated"), RuleScope::File);
    let rules = world.store.dir().join("rules.json");
    let mut trash = TestTrash::new();
    trash.after = Box::new(|_| {
        fs::write(&rules, b"not json").unwrap();
        fs::set_permissions(&rules, fs::Permissions::from_mode(0o600)).unwrap();
    });
    let report = world.run(&[BUNDLE, CACHE], &trash);
    assert!(matches!(
        report.stopped,
        Some(StopReason::ProtectionUnavailable(_))
    ));
    assert_eq!(
        outcomes(&report),
        [ItemOutcome::MovedToTrash, ItemOutcome::NotAttempted]
    );
}
#[test]
fn symlinked_library_and_applications_root_are_refused() {
    let world = World::new();
    swap_for_symlink(&world.home.join("Library"));
    let trash = TestTrash::new();
    let report = world.run(&[CACHE, DATA], &trash);
    assert_eq!(
        outcomes(&report),
        vec![skipped(SkipReason::OutsideRoots); 2]
    );

    let world = World::new();
    swap_for_symlink(&world.apps);
    let report = world.run(&[BUNDLE, CACHE], &trash);
    assert_eq!(
        outcomes(&report),
        [skipped(SkipReason::OutsideRoots), ItemOutcome::NotAttempted]
    );
    assert!(trash.moved().is_empty());
}
#[test]
fn containment_rejects_the_container_itself_and_parent_components() {
    let inside = |p: &str, c: &str| strictly_inside(Path::new(p), Path::new(c));
    assert!(inside("/Applications/A.app", "/Applications"));
    assert!(!inside("/Applications", "/Applications"));
    assert!(!inside("/Applications/../Users/x", "/Applications"));
    assert!(!inside("/ApplicationsX/A.app", "/Applications"));
}
mod cli;
