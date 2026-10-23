//! Tool commands on the desktop: a prune counts what it measurably freed, a
//! requested removal counts as neither freed nor moved, every guard applies,
//! and a failed tool is never retried another way. The fake tools only touch
//! files this test created.

use std::os::unix::fs::PermissionsExt;

use super::removal::{name, planned, Sinks};
use super::*;
use crate::clean::fs_safe::dir_size_checked;
use crate::clean::providers::DesktopTool;

const RUNTIME: &str = "<simulator-runtime:0A8C5D2E-1111-4A1B-8C2D-3E4F5A6B7C8D>";

/// A category cleaned by its own tool. Running it logs `run <name>` into
/// the sinks' log and removes `prunes`, as a prune would.
struct ToolFixture {
    id: &'static str,
    tool: DesktopTool,
    prunes: Option<PathBuf>,
    fail: bool,
    /// Made unreadable by the run, as a folder the tool cannot open stays.
    locks: Option<PathBuf>,
    /// Grows by this many bytes during the run.
    writes: Option<(PathBuf, usize)>,
    app: Option<&'static str>,
    guard: Option<&'static str>,
    log: Arc<Mutex<Vec<String>>>,
}

fn tool(id: &'static str, tool: DesktopTool, sinks: &Sinks) -> ToolFixture {
    ToolFixture {
        id,
        tool,
        prunes: None,
        fail: false,
        locks: None,
        writes: None,
        app: None,
        guard: None,
        log: sinks.log.clone(),
    }
}

impl CleanProvider for ToolFixture {
    fn id(&self) -> &'static str {
        self.id
    }
    fn label(&self) -> &'static str {
        self.id
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
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
    fn roots_from_tool_output(&self) -> bool {
        true
    }
    fn check_item(&self, path: &Path) -> std::result::Result<(), String> {
        match self.guard {
            Some(guard) if path.to_string_lossy().contains(guard) => Err("refused".into()),
            _ => Ok(()),
        }
    }
    fn desktop_tool(&self) -> Option<DesktopTool> {
        Some(self.tool)
    }
    fn run_desktop_tool(&self, item: &CleanItem) -> std::result::Result<(), String> {
        let entry = match item.path.is_absolute() {
            true => name(&item.path),
            false => item.path.to_string_lossy().into_owned(),
        };
        self.log.lock().unwrap().push(format!("run {entry}"));
        if self.fail {
            return Err("tool failed".into());
        }
        if let Some(file) = &self.prunes {
            fs::remove_file(file).unwrap();
        }
        if let Some(dir) = &self.locks {
            fs::set_permissions(dir, fs::Permissions::from_mode(0o000)).unwrap();
        }
        if let Some((file, len)) = &self.writes {
            fs::write(file, vec![7u8; *len]).unwrap();
        }
        Ok(())
    }
}

fn run_tools(
    store: &TempStore,
    home: &Path,
    plan: &[PlannedItem],
    providers: Vec<Box<dyn CleanProvider>>,
    probe: &dyn AppProbe,
    sinks: &Sinks,
) -> CheckedExecReport {
    let lease = store.store.lease().unwrap();
    let ctx = ExecContext {
        providers: &providers,
        home: Some(home),
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

/// `root/cache` holding `one`, `two` and `three`, planned as a prune with
/// the size and flags discovery measures.
fn cache(root: &Path) -> PlannedItem {
    let cache = root.join("cache");
    fs::create_dir(&cache).unwrap();
    for file in ["one", "two", "three"] {
        fs::write(cache.join(file), vec![7u8; 20_000]).unwrap();
    }
    let ctx = ScanContext::unchecked();
    let mut planned = planned("uv", root, "cache", Removal::RunTool(DesktopTool::Prune));
    planned.item.size = dir_size_checked(&cache, &ctx);
    planned.size_is_upper_bound = true;
    planned
}

/// A simulator runtime: a placeholder that names no file.
fn runtime(size: u64) -> PlannedItem {
    PlannedItem {
        item: CleanItem {
            category_id: "simulator-runtimes".into(),
            category_label: "simulator-runtimes".into(),
            path: PathBuf::from(RUNTIME),
            size,
            risk: RiskLevel::Review,
            evidence: Vec::new(),
        },
        fingerprint: None,
        roots: Vec::new(),
        covers: Vec::new(),
        removal: Removal::RunTool(DesktopTool::Request),
        size_is_upper_bound: true,
        shared: Default::default(),
    }
}

fn allocated(path: &Path) -> u64 {
    fs::symlink_metadata(path).unwrap().blocks() * 512
}

fn pruned_outcome(report: &CheckedExecReport) -> (u64, bool) {
    match &outcomes(report)[0] {
        ItemOutcome::Pruned {
            bytes_freed,
            upper_bound,
        } => (*bytes_freed, *upper_bound),
        other => panic!("not pruned: {other:?}"),
    }
}

fn prune_with(uv: ToolFixture, root: &Path, sinks: &Sinks) -> CheckedExecReport {
    run_tools(
        &TempStore::new(),
        root,
        &[cache(root)],
        vec![Box::new(uv)],
        &MockChecker::none(),
        sinks,
    )
}

#[test]
fn a_prune_followed_by_an_unreadable_folder_is_only_an_upper_bound() {
    let root = fixture_root("prune-unreadable", &[]);
    let sinks = Sinks::default();
    let mut uv = tool("uv", DesktopTool::Prune, &sinks);
    uv.prunes = Some(root.join("cache/one"));
    let report = prune_with(uv, &root, &sinks);
    assert!(
        !pruned_outcome(&report).1,
        "a readable cache is measured exactly"
    );
    assert!(!report.freed_is_upper_bound);

    // The tool leaves a folder it cannot open: `after` misses what is in it,
    // so before minus after may be more than was freed.
    let root = fixture_root("prune-unreadable-after", &[]);
    let locked = root.join("cache/locked");
    let mut uv = tool("uv", DesktopTool::Prune, &sinks);
    uv.prunes = Some(root.join("cache/one"));
    uv.locks = Some(locked.clone());
    let planned = cache(&root);
    fs::create_dir(&locked).unwrap();
    fs::write(locked.join("hidden"), vec![7u8; 20_000]).unwrap();
    let report = run_tools(
        &TempStore::new(),
        &root,
        &[planned],
        vec![Box::new(uv)],
        &MockChecker::none(),
        &sinks,
    );
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(pruned_outcome(&report).1, "{:?}", outcomes(&report));
    assert!(report.freed_is_upper_bound);
    let _ = remove_recursive_safe(&root);
}

#[test]
fn a_cache_that_grew_during_the_prune_frees_nothing() {
    let root = fixture_root("prune-grew", &[]);
    let sinks = Sinks::default();
    let mut uv = tool("uv", DesktopTool::Prune, &sinks);
    uv.writes = Some((root.join("cache/new"), 100_000));
    let report = prune_with(uv, &root, &sinks);
    assert_eq!(
        pruned_outcome(&report).0,
        0,
        "never negative, never wrapped"
    );
    assert_eq!(report.bytes_freed, 0);
    let _ = remove_recursive_safe(&root);
}

#[test]
fn a_prune_runs_the_tool_and_counts_only_what_it_freed() {
    let root = fixture_root("prune", &["gone"]);
    let sinks = Sinks::default();
    let mut uv = tool("uv", DesktopTool::Prune, &sinks);
    uv.prunes = Some(root.join("cache/one"));
    let plan = [
        cache(&root),
        planned("user-logs", &root, "gone", Removal::Delete),
    ];
    let one = allocated(&root.join("cache/one"));
    let report = run_tools(
        &TempStore::new(),
        &root,
        &plan,
        vec![Box::new(uv), Box::new(fixture("user-logs"))],
        &MockChecker::none(),
        &sinks,
    );
    let outcomes = outcomes(&report);
    let pruned = match &outcomes[0] {
        ItemOutcome::Pruned { bytes_freed, .. } => *bytes_freed,
        other => panic!("not pruned: {other:?}"),
    };
    assert_eq!(pruned, one, "before minus after");
    assert!(pruned < plan[0].item.size);
    assert_eq!(outcomes[1], ItemOutcome::Deleted);
    assert_eq!(
        report.bytes_freed,
        pruned + 4,
        "measured, not the discovery size"
    );
    assert_eq!(sinks.log(), ["delete gone", "run cache"]);
    let _ = remove_recursive_safe(&root);
}

#[test]
fn a_runtime_removal_is_requested_and_counts_as_neither_freed_nor_moved() {
    let root = fixture_root("request", &["kept"]);
    let store = TempStore::new();
    // A saved rule, so a rules check on the placeholder would really run.
    store.keep(&root.join("kept"), RuleScope::File);
    let sinks = Sinks::default();
    let simctl = tool("simulator-runtimes", DesktopTool::Request, &sinks);
    let report = run_tools(
        &store,
        &root,
        &[runtime(8_000)],
        vec![Box::new(simctl)],
        &MockChecker::none(),
        &sinks,
    );
    assert_eq!(outcomes(&report), [ItemOutcome::RemovalRequested]);
    assert_eq!(report.bytes_requested(), 8_000);
    assert_eq!(report.bytes_freed, 0);
    assert_eq!(report.bytes_moved_to_trash(), 0);
    assert_eq!(sinks.log(), [format!("run {RUNTIME}")]);
    let _ = remove_recursive_safe(&root);
}

#[test]
fn a_failed_tool_is_never_retried_another_way() {
    let root = fixture_root("tool-fails", &[]);
    let sinks = Sinks::default();
    let mut uv = tool("uv", DesktopTool::Prune, &sinks);
    uv.fail = true;
    let mut simctl = tool("simulator-runtimes", DesktopTool::Request, &sinks);
    simctl.fail = true;
    let report = run_tools(
        &TempStore::new(),
        &root,
        &[cache(&root), runtime(8_000)],
        vec![Box::new(uv), Box::new(simctl)],
        &MockChecker::none(),
        &sinks,
    );
    let failed = ItemOutcome::ToolFailed {
        detail: "tool failed".into(),
    };
    assert_eq!(outcomes(&report), [failed.clone(), failed]);
    assert_eq!(
        sinks.log(),
        ["run cache".to_string(), format!("run {RUNTIME}")],
        "one call each: no deletion, no Trash, no elevation, no second attempt"
    );
    assert_eq!((report.bytes_freed, report.bytes_requested()), (0, 0));
    assert!(root.join("cache/one").exists());
    let _ = remove_recursive_safe(&root);
}

#[test]
fn tool_commands_keep_every_guard() {
    let root = fixture_root("tool-guards", &[]);
    let sinks = Sinks::default();
    let mut uv = tool("uv", DesktopTool::Prune, &sinks);
    uv.app = Some("uv");
    let report = run_tools(
        &TempStore::new(),
        &root,
        &[cache(&root)],
        vec![Box::new(uv)],
        &MockChecker::with_running(["uv"]),
        &sinks,
    );
    assert_eq!(
        outcomes(&report),
        [ItemOutcome::Skipped(SkipReason::AppRunning("uv".into()))]
    );

    let mut simctl = tool("simulator-runtimes", DesktopTool::Request, &sinks);
    simctl.guard = Some("simulator-runtime");
    let report = run_tools(
        &TempStore::new(),
        &root,
        &[runtime(8_000)],
        vec![Box::new(simctl)],
        &MockChecker::none(),
        &sinks,
    );
    assert_eq!(
        outcomes(&report),
        [ItemOutcome::Skipped(SkipReason::ProviderGuard(
            "refused".into()
        ))]
    );

    let store = TempStore::new();
    store.keep(&root.join("cache"), RuleScope::Directory);
    let stale = PlannedItem {
        fingerprint: fingerprint(&root).ok(),
        ..planned("uv", &root, "cache", Removal::RunTool(DesktopTool::Prune))
    };
    let providers = || -> Vec<Box<dyn CleanProvider>> {
        vec![Box::new(tool("uv", DesktopTool::Prune, &sinks))]
    };
    let report = run_tools(
        &store,
        &root,
        &[planned(
            "uv",
            &root,
            "cache",
            Removal::RunTool(DesktopTool::Prune),
        )],
        providers(),
        &MockChecker::none(),
        &sinks,
    );
    assert!(matches!(
        outcomes(&report)[0],
        ItemOutcome::Skipped(SkipReason::Kept { .. })
    ));
    let report = run_tools(
        &TempStore::new(),
        &root,
        &[stale],
        providers(),
        &MockChecker::none(),
        &sinks,
    );
    assert_eq!(
        outcomes(&report),
        [ItemOutcome::Skipped(SkipReason::Changed)]
    );
    assert!(sinks.log().is_empty(), "{:?}", sinks.log());
    let _ = remove_recursive_safe(&root);
}

#[test]
fn only_a_categorys_own_tool_runs_and_its_store_is_never_deleted() {
    let root = fixture_root("tool-mismatch", &["log"]);
    let sinks = Sinks::default();
    let providers: Vec<Box<dyn CleanProvider>> = vec![
        Box::new(tool("uv", DesktopTool::Prune, &sinks)),
        Box::new(tool("simulator-runtimes", DesktopTool::Request, &sinks)),
        Box::new(fixture("user-logs")),
    ];
    let pruned_log = planned(
        "user-logs",
        &root,
        "log",
        Removal::RunTool(DesktopTool::Prune),
    );
    let requested_cache = PlannedItem {
        removal: Removal::RunTool(DesktopTool::Request),
        ..cache(&root)
    };
    let deleted_cache = PlannedItem {
        removal: Removal::Delete,
        ..requested_cache.clone()
    };
    let trashed_cache = PlannedItem {
        removal: Removal::MoveToTrash,
        ..requested_cache.clone()
    };
    let trashed_runtime = PlannedItem {
        removal: Removal::MoveToTrash,
        ..runtime(8_000)
    };
    let deleted_runtime = PlannedItem {
        removal: Removal::Delete,
        ..runtime(8_000)
    };
    let report = run_tools(
        &TempStore::new(),
        &root,
        &[
            pruned_log,
            requested_cache,
            deleted_cache,
            trashed_runtime,
            deleted_runtime,
            trashed_cache,
        ],
        providers,
        &MockChecker::none(),
        &sinks,
    );
    let report_only = ItemOutcome::Skipped(SkipReason::ReportOnly);
    let missing = ItemOutcome::Skipped(SkipReason::Missing);
    assert_eq!(
        outcomes(&report),
        [
            report_only.clone(),
            report_only.clone(),
            report_only,
            missing.clone(),
            missing,
            ItemOutcome::MovedToTrash,
        ],
        "a prune's folder may only go to the Trash besides its tool"
    );
    assert_eq!(sinks.log(), ["trash cache"]);
    let _ = remove_recursive_safe(&root);
}
