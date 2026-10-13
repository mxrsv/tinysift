use super::*;
use crate::clean::fs_safe::{fingerprint, remove_recursive_safe};
use crate::clean::process::test_support::{FailingProbe, MockChecker};
use crate::clean::protection::{ProtectionRule, RuleScope, RulesDocument};
use crate::clean::protection_store::ProtectionStore;
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{ExecAction, ExecReport, RiskLevel};
use crate::error::Result;
use std::path::PathBuf;
use std::sync::Mutex;

mod protection;

struct Fixture {
    id: &'static str,
    desktop: bool,
    risk: RiskLevel,
    app: Option<&'static str>,
    guard: Option<&'static str>,
    tool: bool,
}

fn fixture(id: &'static str) -> Fixture {
    Fixture {
        id,
        desktop: true,
        risk: RiskLevel::Safe,
        app: None,
        guard: None,
        tool: false,
    }
}

impl CleanProvider for Fixture {
    fn id(&self) -> &'static str {
        self.id
    }
    fn label(&self) -> &'static str {
        self.id
    }
    fn risk(&self) -> RiskLevel {
        self.risk
    }
    fn inclusion_reason(&self) -> String {
        "fixture".into()
    }
    fn requires_app_quit(&self) -> Option<&'static str> {
        self.app
    }
    fn discover(&self, _: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        Ok(Vec::new())
    }
    fn execute(&self, _: &[CleanItem], _: ExecAction) -> Result<ExecReport> {
        panic!("checked execution must never call provider.execute");
    }
    fn desktop_trash_paths(&self) -> bool {
        self.desktop
    }
    fn roots_from_tool_output(&self) -> bool {
        self.tool
    }
    fn check_item(&self, path: &Path) -> std::result::Result<(), String> {
        match self.guard {
            Some(name) if path.ends_with(name) => Err(format!("{name} is protected")),
            _ => Ok(()),
        }
    }
}

/// Records each move; fails with `fail` for paths ending in `fail_on`.
#[derive(Default)]
struct FakeTrash {
    moved: Mutex<Vec<PathBuf>>,
    fail_on: Option<(&'static str, TrashError)>,
}

impl Trash for FakeTrash {
    fn move_to_trash(&self, path: &Path) -> std::result::Result<(), TrashError> {
        if let Some((name, error)) = &self.fail_on {
            if path.ends_with(name) {
                return Err(error.clone());
            }
        }
        self.moved.lock().unwrap().push(path.to_path_buf());
        Ok(())
    }
}

fn fixture_root(label: &str, names: &[&str]) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tiny-checked-exec-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&dir).unwrap();
    for name in names {
        fs::write(dir.join(name), b"1234").unwrap();
    }
    dir
}

fn plan(category: &str, root: &Path, name: &str) -> PlannedItem {
    let path = root.join(name);
    PlannedItem {
        fingerprint: fingerprint(&path).unwrap(),
        item: CleanItem {
            category_id: category.into(),
            category_label: category.into(),
            size: 4,
            path,
            risk: RiskLevel::Safe,
            evidence: Vec::new(),
        },
        roots: vec![root.to_path_buf()],
        covers: Vec::new(),
    }
}

/// A protection store in its own temp directory, never inside or above a
/// fixture root (its internal rules would keep those from moving).
struct TempStore {
    root: PathBuf,
    store: ProtectionStore,
}

impl TempStore {
    fn new() -> Self {
        let root = fixture_root("store", &[]);
        let store = ProtectionStore::at(root.join("clean-protection"));
        Self { root, store }
    }

    /// Saves a rule, as an earlier session did.
    fn keep(&self, path: &Path, scope: RuleScope) {
        let mut lease = self.store.lease().unwrap();
        let revision = lease.snapshot().revision;
        lease.add(revision, path, scope).unwrap();
    }

    fn rules_file(&self) -> PathBuf {
        self.store.dir().join("rules.json")
    }

    /// Rewrites the rules as another process would: atomically, owner-only.
    fn rewrite(&self, edit: impl FnOnce(&mut RulesDocument)) {
        use std::os::unix::fs::PermissionsExt;
        let mut doc: RulesDocument =
            serde_json::from_slice(&fs::read(self.rules_file()).unwrap()).unwrap();
        edit(&mut doc);
        let staged = self.root.join("rules.next");
        fs::write(&staged, serde_json::to_vec(&doc).unwrap()).unwrap();
        fs::set_permissions(&staged, fs::Permissions::from_mode(0o600)).unwrap();
        fs::rename(&staged, self.rules_file()).unwrap();
    }
}

impl Drop for TempStore {
    fn drop(&mut self) {
        let _ = remove_recursive_safe(&self.root);
    }
}

fn run_with(
    store: &TempStore,
    plan: &[PlannedItem],
    providers: Vec<Box<dyn CleanProvider>>,
    probe: &dyn AppProbe,
    trash: &dyn Trash,
    cancel: Option<&AtomicBool>,
) -> CheckedExecReport {
    let home = std::env::temp_dir();
    let lease = store.store.lease().unwrap();
    let ctx = ExecContext {
        providers: &providers,
        home: Some(&home),
        probe,
        trash,
        cancel,
        progress: None,
        policy: &lease,
    };
    execute_checked(plan, &ctx)
}

/// Runs against a store that holds no rules.
fn run(
    plan: &[PlannedItem],
    providers: Vec<Box<dyn CleanProvider>>,
    probe: &dyn AppProbe,
    trash: &dyn Trash,
    cancel: Option<&AtomicBool>,
) -> CheckedExecReport {
    run_with(&TempStore::new(), plan, providers, probe, trash, cancel)
}

fn outcomes(report: &CheckedExecReport) -> Vec<ItemOutcome> {
    report.results.iter().map(|r| r.outcome.clone()).collect()
}

#[test]
fn empty_plan_touches_nothing() {
    let trash = FakeTrash::default();
    let report = run(&[], vec![], &MockChecker::none(), &trash, None);
    assert!(report.results.is_empty());
    assert!(trash.moved.lock().unwrap().is_empty());
}

#[test]
fn moves_valid_items_and_reports_bytes_without_claiming_freed_space() {
    let root = fixture_root("ok", &["a", "b"]);
    let items = [plan("logs", &root, "a"), plan("logs", &root, "b")];
    let trash = FakeTrash::default();
    let report = run(
        &items,
        vec![Box::new(fixture("logs"))],
        &MockChecker::none(),
        &trash,
        None,
    );
    assert_eq!(outcomes(&report), vec![ItemOutcome::MovedToTrash; 2]);
    assert_eq!(report.bytes_selected(), 8);
    assert_eq!(report.bytes_moved_to_trash(), 8);
    assert_eq!(trash.moved.lock().unwrap().len(), 2);
    let _ = remove_recursive_safe(&root);
}

#[test]
fn report_only_and_destructive_categories_are_refused() {
    let root = fixture_root("report-only", &["images", "snap"]);
    let mut docker = fixture("docker");
    docker.desktop = false;
    let mut destructive = fixture("tm");
    destructive.risk = RiskLevel::Destructive;
    let items = [plan("docker", &root, "images"), plan("tm", &root, "snap")];
    let trash = FakeTrash::default();
    let report = run(
        &items,
        vec![Box::new(docker), Box::new(destructive)],
        &MockChecker::none(),
        &trash,
        None,
    );
    assert_eq!(
        outcomes(&report),
        vec![ItemOutcome::Skipped(SkipReason::ReportOnly); 2]
    );
    assert!(trash.moved.lock().unwrap().is_empty());
    let _ = remove_recursive_safe(&root);
}

#[test]
fn symlinks_replacements_and_missing_paths_are_skipped() {
    let root = fixture_root("swap", &["edited", "replaced", "gone", "target"]);
    let items = [
        plan("logs", &root, "edited"),
        plan("logs", &root, "replaced"),
        plan("logs", &root, "gone"),
        plan("logs", &root, "target"),
    ];
    fs::write(root.join("edited"), b"longer than before").unwrap();
    fs::write(root.join("replacement"), b"1234").unwrap();
    fs::rename(root.join("replacement"), root.join("replaced")).unwrap();
    remove_recursive_safe(&root.join("gone")).unwrap();
    fs::rename(root.join("target"), root.join("moved")).unwrap();
    std::os::unix::fs::symlink(root.join("moved"), root.join("target")).unwrap();

    let trash = FakeTrash::default();
    let report = run(
        &items,
        vec![Box::new(fixture("logs"))],
        &MockChecker::none(),
        &trash,
        None,
    );
    assert_eq!(
        outcomes(&report),
        vec![
            ItemOutcome::Skipped(SkipReason::Changed),
            ItemOutcome::Skipped(SkipReason::Changed),
            ItemOutcome::Skipped(SkipReason::Missing),
            ItemOutcome::Skipped(SkipReason::Symlink),
        ]
    );
    assert!(trash.moved.lock().unwrap().is_empty());
    let _ = remove_recursive_safe(&root);
}

#[test]
fn a_symlinked_directory_on_the_way_from_the_root_is_refused() {
    let root = fixture_root("ancestor", &[]);
    fs::create_dir(root.join("sub")).unwrap();
    fs::write(root.join("sub/f"), b"1234").unwrap();
    let item = plan("logs", &root, "sub/f");
    // Swap `sub` for a symlink to a directory holding the same file.
    let elsewhere = fixture_root("elsewhere", &[]);
    fs::rename(root.join("sub"), elsewhere.join("sub")).unwrap();
    std::os::unix::fs::symlink(elsewhere.join("sub"), root.join("sub")).unwrap();

    let trash = FakeTrash::default();
    let report = run(
        &[item],
        vec![Box::new(fixture("logs"))],
        &MockChecker::none(),
        &trash,
        None,
    );
    assert_eq!(
        outcomes(&report),
        vec![ItemOutcome::Skipped(SkipReason::OutsideRoots)]
    );
    let _ = remove_recursive_safe(&root);
    let _ = remove_recursive_safe(&elsewhere);
}

#[test]
fn paths_outside_the_discovery_roots_are_refused() {
    let root = fixture_root("roots", &["a"]);
    let mut item = plan("logs", &root, "a");
    item.roots = vec![root.join("elsewhere")];
    let trash = FakeTrash::default();
    let report = run(
        &[item],
        vec![Box::new(fixture("logs"))],
        &MockChecker::none(),
        &trash,
        None,
    );
    assert_eq!(
        outcomes(&report),
        vec![ItemOutcome::Skipped(SkipReason::OutsideRoots)]
    );
    let _ = remove_recursive_safe(&root);
}

#[test]
fn provider_guard_running_apps_and_failed_probes_refuse_items() {
    let root = fixture_root("guards", &["registry", "bin"]);
    let items = [
        plan("cargo", &root, "registry"),
        plan("cargo", &root, "bin"),
        plan("xcode", &root, "registry"),
    ];
    let trash = FakeTrash::default();
    let providers = || -> Vec<Box<dyn CleanProvider>> {
        let mut guarded = fixture("cargo");
        guarded.guard = Some("bin");
        let mut gated = fixture("xcode");
        gated.app = Some("Xcode");
        vec![Box::new(guarded), Box::new(gated)]
    };
    let running = MockChecker::with_running(["Xcode"]);
    let report = run(&items, providers(), &running, &trash, None);
    assert_eq!(
        outcomes(&report),
        vec![
            ItemOutcome::MovedToTrash,
            ItemOutcome::Skipped(SkipReason::ProviderGuard("bin is protected".into())),
            ItemOutcome::Skipped(SkipReason::AppRunning("Xcode".into())),
        ]
    );

    let report = run(&items[2..], providers(), &FailingProbe, &trash, None);
    assert!(matches!(
        &outcomes(&report)[0],
        ItemOutcome::Skipped(SkipReason::SafetyCheckFailed(detail)) if detail.contains("Xcode")
    ));
    let _ = remove_recursive_safe(&root);
}

#[test]
fn home_its_ancestors_and_tool_paths_outside_home_are_refused() {
    let home = fixture_root("home", &[]);
    let outside = fixture_root("outside", &["cache"]);
    let mut at_home = plan("logs", &home, "");
    at_home.item.path = home.clone();
    at_home.fingerprint = fingerprint(&home).unwrap();
    let tool_outside = plan("npm", &outside, "cache");
    let plain_outside = plan("logs", &outside, "cache");
    let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(fixture("logs")), {
        let mut npm = fixture("npm");
        npm.tool = true;
        Box::new(npm)
    }];
    let trash = FakeTrash::default();
    let store = TempStore::new();
    let lease = store.store.lease().unwrap();
    let ctx = ExecContext {
        providers: &providers,
        home: Some(&home),
        probe: &MockChecker::none(),
        trash: &trash,
        cancel: None,
        progress: None,
        policy: &lease,
    };
    let report = execute_checked(&[at_home, tool_outside, plain_outside], &ctx);
    assert_eq!(
        outcomes(&report),
        vec![
            ItemOutcome::Skipped(SkipReason::ProtectedPath("the home folder".into())),
            ItemOutcome::Skipped(SkipReason::ProtectedPath(
                "a tool reported a path outside the home folder".into()
            )),
            ItemOutcome::MovedToTrash,
        ]
    );
    let _ = remove_recursive_safe(&home);
    let _ = remove_recursive_safe(&outside);
}

#[test]
fn items_on_another_volume_than_home_are_refused() {
    let root = fixture_root("volume", &["a"]);
    let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(fixture("logs"))];
    let trash = FakeTrash::default();
    // devfs is a different device than the temp dir's volume.
    let store = TempStore::new();
    let lease = store.store.lease().unwrap();
    let ctx = ExecContext {
        providers: &providers,
        home: Some(Path::new("/dev")),
        probe: &MockChecker::none(),
        trash: &trash,
        cancel: None,
        progress: None,
        policy: &lease,
    };
    let report = execute_checked(&[plan("logs", &root, "a")], &ctx);
    assert_eq!(
        outcomes(&report),
        vec![ItemOutcome::Skipped(SkipReason::NotOnHomeVolume)]
    );
    let unknown = ExecContext { home: None, ..ctx };
    let report = execute_checked(&[plan("logs", &root, "a")], &unknown);
    assert!(matches!(
        &outcomes(&report)[0],
        ItemOutcome::Skipped(SkipReason::SafetyCheckFailed(_))
    ));
    assert!(trash.moved.lock().unwrap().is_empty());
    let _ = remove_recursive_safe(&root);
}

#[test]
fn merged_items_keep_their_guards_and_app_gates() {
    let root = fixture_root("merged", &["cache", "bin"]);
    let providers = || -> Vec<Box<dyn CleanProvider>> {
        let mut guarded = fixture("cargo");
        guarded.guard = Some("bin");
        let mut gated = fixture("streaming");
        gated.app = Some("Spotify");
        vec![
            Box::new(fixture("caches")),
            Box::new(guarded),
            Box::new(gated),
        ]
    };
    // A duplicate found by an app-gated category.
    let mut duplicate = plan("caches", &root, "cache");
    duplicate.covers = vec![plan("streaming", &root, "cache").item];
    // A path covering a guarded one.
    let mut covering = plan("caches", &root, "cache");
    covering.covers = vec![plan("cargo", &root, "bin").item];
    let trash = FakeTrash::default();
    let running = MockChecker::with_running(["Spotify"]);
    let report = run(&[duplicate, covering], providers(), &running, &trash, None);
    assert_eq!(
        outcomes(&report),
        vec![
            ItemOutcome::Skipped(SkipReason::AppRunning("Spotify".into())),
            ItemOutcome::Skipped(SkipReason::ProviderGuard("bin is protected".into())),
        ]
    );
    assert!(trash.moved.lock().unwrap().is_empty());
    let _ = remove_recursive_safe(&root);
}

#[test]
fn cancellation_reports_completed_work_and_leaves_the_rest() {
    let root = fixture_root("cancel", &["a", "b", "c"]);
    let items = [
        plan("logs", &root, "a"),
        plan("logs", &root, "b"),
        plan("logs", &root, "c"),
    ];
    let flag = AtomicBool::new(false);
    /// Cancels after the first move.
    struct CancellingTrash<'a>(&'a AtomicBool, FakeTrash);
    impl Trash for CancellingTrash<'_> {
        fn move_to_trash(&self, path: &Path) -> std::result::Result<(), TrashError> {
            self.0.store(true, Ordering::Release);
            self.1.move_to_trash(path)
        }
    }
    let trash = CancellingTrash(&flag, FakeTrash::default());
    let report = run(
        &items,
        vec![Box::new(fixture("logs"))],
        &MockChecker::none(),
        &trash,
        Some(&flag),
    );
    assert_eq!(report.stopped, Some(StopReason::Cancelled));
    assert_eq!(
        outcomes(&report),
        vec![
            ItemOutcome::MovedToTrash,
            ItemOutcome::NotAttempted,
            ItemOutcome::NotAttempted
        ]
    );
    assert_eq!(report.bytes_moved_to_trash(), 4);
    assert!(root.join("b").exists() && root.join("c").exists());
    let _ = remove_recursive_safe(&root);
}

#[test]
fn the_app_probe_runs_before_the_path_checks() {
    let root = fixture_root("order", &["gone"]);
    let item = plan("logs", &root, "gone");
    remove_recursive_safe(&root.join("gone")).unwrap();
    let mut gated = fixture("logs");
    gated.app = Some("Xcode");
    let running = MockChecker::with_running(["Xcode"]);
    let trash = FakeTrash::default();
    let report = run(&[item], vec![Box::new(gated)], &running, &trash, None);
    assert_eq!(
        outcomes(&report),
        vec![ItemOutcome::Skipped(SkipReason::AppRunning("Xcode".into()))],
        "the missing path is checked after the probe"
    );
    let _ = remove_recursive_safe(&root);
}

#[test]
fn cancellation_is_checked_before_validation_too() {
    let root = fixture_root("cancel-first", &["a", "b"]);
    let items = [plan("logs", &root, "a"), plan("logs", &root, "b")];
    let flag = AtomicBool::new(true);
    let mut gated = fixture("logs");
    gated.app = Some("Xcode");
    let trash = FakeTrash::default();
    // A failing probe would turn a validated item into a skip.
    let report = run(
        &items,
        vec![Box::new(gated)],
        &FailingProbe,
        &trash,
        Some(&flag),
    );
    assert_eq!(report.stopped, Some(StopReason::Cancelled));
    assert_eq!(outcomes(&report), vec![ItemOutcome::NotAttempted; 2]);
    let _ = remove_recursive_safe(&root);
}

#[test]
fn automation_denial_stops_and_trash_failures_keep_the_source() {
    let root = fixture_root("denied", &["a", "b", "c"]);
    let items = [
        plan("logs", &root, "a"),
        plan("logs", &root, "b"),
        plan("logs", &root, "c"),
    ];
    let failing = FakeTrash {
        fail_on: Some(("a", TrashError::Failed("Finder error".into()))),
        ..Default::default()
    };
    let report = run(
        &items[..2],
        vec![Box::new(fixture("logs"))],
        &MockChecker::none(),
        &failing,
        None,
    );
    assert_eq!(
        outcomes(&report),
        vec![
            ItemOutcome::Failed {
                error: TrashError::Failed("Finder error".into()),
                source_present: true,
            },
            ItemOutcome::MovedToTrash,
        ],
        "a Trash failure is per item and never deletes"
    );
    assert!(root.join("a").exists());

    let denied = FakeTrash {
        fail_on: Some(("c", TrashError::AutomationDenied("(-1743)".into()))),
        ..Default::default()
    };
    let report = run(
        &[items[2].clone(), plan("logs", &root, "a")],
        vec![Box::new(fixture("logs"))],
        &MockChecker::none(),
        &denied,
        None,
    );
    assert_eq!(report.stopped, Some(StopReason::AutomationDenied));
    assert_eq!(outcomes(&report)[1], ItemOutcome::NotAttempted);
    assert!(denied.moved.lock().unwrap().is_empty());
    let _ = remove_recursive_safe(&root);
}
