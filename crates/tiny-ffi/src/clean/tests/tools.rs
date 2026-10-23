//! Tool commands over FFI: off unless the app asks; a prune in Permanent
//! mode and a requested runtime removal are reported apart from freed and
//! moved bytes; nothing is retried another way. The fake tools only touch
//! files the test created; no real `uv`, `pnpm` or `simctl` runs.

use std::fs;
use std::sync::Mutex;

use tiny_core::clean::providers::DesktopTool;
use tiny_core::clean::types::{ExecAction, ExecReport};
use tiny_core::error::Result as CoreResult;

use super::*;

const RUNTIME: &str = "<simulator-runtime:0A8C5D2E-1111-4A1B-8C2D-3E4F5A6B7C8D>";
const RUNTIME_BYTES: u64 = 8_000_000_000;

/// A category cleaned by its own tool, like uv or simulator runtimes.
/// Running it logs `run <name>` and removes `prunes`, as a prune would.
struct ToolFixture {
    id: &'static str,
    tool: DesktopTool,
    path: PathBuf,
    prunes: Option<PathBuf>,
    fail: bool,
    log: Arc<Mutex<Vec<String>>>,
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
    fn discover(&self, ctx: &ScanContext<'_>) -> CoreResult<Vec<CleanItem>> {
        let size = if self.path.is_absolute() {
            ctx.add_root(&self.path);
            tiny_core::clean::fs_safe::dir_size_checked(&self.path, ctx)
        } else {
            RUNTIME_BYTES
        };
        // A prune frees at most the cache; simctl's figure is not blocks.
        ctx.note_size_upper_bound(&self.path);
        Ok(vec![CleanItem {
            category_id: self.id.into(),
            category_label: self.id.into(),
            path: self.path.clone(),
            size,
            risk: RiskLevel::Review,
            evidence: Vec::new(),
        }])
    }
    fn execute(&self, _: &[CleanItem], _: ExecAction) -> CoreResult<ExecReport> {
        panic!("the desktop path must never call provider.execute");
    }
    fn desktop_tool(&self) -> Option<DesktopTool> {
        Some(self.tool)
    }
    fn run_desktop_tool(&self, item: &CleanItem) -> std::result::Result<(), String> {
        let name = item.path.file_name().unwrap_or(item.path.as_os_str());
        let name = name.to_string_lossy();
        self.log.lock().unwrap().push(format!("run {name}"));
        if self.fail {
            return Err("tool failed".into());
        }
        if let Some(file) = &self.prunes {
            fs::remove_file(file).unwrap();
        }
        Ok(())
    }
}

/// `cache` (the uv fixture prunes `one` of its three files), a simulator
/// runtime, and `downloads/d` (Your files).
struct Layout {
    dir: PathBuf,
    log: Arc<Mutex<Vec<String>>>,
    fail: bool,
}

impl Layout {
    fn new(label: &str) -> Self {
        let dir = fixture_dir(label, &[], 0);
        fs::create_dir_all(dir.join("cache")).unwrap();
        for file in ["one", "two", "three"] {
            fs::write(dir.join("cache").join(file), vec![7u8; 20_000]).unwrap();
        }
        fs::create_dir(dir.join("downloads")).unwrap();
        fs::write(dir.join("downloads/d"), b"1234").unwrap();
        Self {
            dir,
            log: Arc::default(),
            fail: false,
        }
    }

    fn providers(&self) -> Vec<Box<dyn CleanProvider>> {
        let tool = |id, tool, path| ToolFixture {
            id,
            tool,
            path,
            prunes: None,
            fail: self.fail,
            log: self.log.clone(),
        };
        let uv = ToolFixture {
            prunes: Some(self.dir.join("cache/one")),
            ..tool("uv", DesktopTool::Prune, self.dir.join("cache"))
        };
        let runtimes = tool(
            "simulator-runtimes",
            DesktopTool::Request,
            PathBuf::from(RUNTIME),
        );
        vec![
            Box::new(uv),
            Box::new(runtimes),
            Box::new(Fixture::new("downloads-old", &self.dir.join("downloads"))),
        ]
    }

    fn log(&self) -> Vec<String> {
        self.log.lock().unwrap().clone()
    }

    fn remove(
        &self,
        session: &TinySession,
        preview_id: &str,
        sinks: &FakeTrash,
    ) -> Result<FfiRemovalReport, FfiError> {
        let providers = |_: &CleanOptions| self.providers();
        session.remove_clean(
            preview_id,
            &providers,
            &NotRunning,
            sinks,
            sinks,
            sinks,
            &CancellationToken::default(),
            &Progress::default(),
        )
    }
}

fn with_tools() -> CleanOptions {
    CleanOptions {
        tool_commands: true,
        ..CleanOptions::default()
    }
}

fn actions(discovery: &FfiDiscovery) -> Vec<(&str, FfiDesktopAction)> {
    discovery
        .categories
        .iter()
        .map(|c| (c.id.as_str(), c.desktop_action))
        .collect()
}

fn by_path<'a>(preview: &'a FfiPreview, path: &str) -> &'a FfiPreviewItem {
    preview
        .items
        .iter()
        .find(|i| i.path.ends_with(path))
        .unwrap()
}

fn outcome<'a>(report: &'a FfiRemovalReport, path: &str) -> &'a FfiRemovalOutcome {
    let result = report.results.iter().find(|r| r.path.ends_with(path));
    &result.unwrap().outcome
}

#[test]
fn tool_categories_stay_report_only_unless_the_app_asks() {
    let layout = Layout::new("tools-off");
    let session = TestSession::new();
    let discovery = discover(&session, &layout.providers()).unwrap();
    let report_only = FfiDesktopAction::ReportOnly {
        reason: FfiReportOnlyReason::NotPerPathTrash,
    };
    assert_eq!(
        actions(&discovery),
        [
            ("uv", report_only),
            ("simulator-runtimes", report_only),
            ("downloads-old", FfiDesktopAction::MoveToTrash),
        ]
    );
    let runtime = candidate_ids(&discovery, "simulator-runtimes");
    assert!(matches!(
        session.clean_preview(runtime),
        Err(FfiError::InvalidInput { .. })
    ));

    let discovery = discover_with(&session, &layout.providers(), with_tools()).unwrap();
    assert_eq!(
        actions(&discovery),
        [
            ("uv", FfiDesktopAction::MoveToTrash),
            ("simulator-runtimes", FfiDesktopAction::RequestRemoval),
            ("downloads-old", FfiDesktopAction::MoveToTrash),
        ]
    );
    remove(&layout.dir);
}

#[test]
fn permanent_mode_prunes_requests_and_trashes_and_counts_each_apart() {
    let layout = Layout::new("tools-permanent");
    let session = TestSession::new();
    // A saved rule, so the rules really run over every selected item.
    let discovery = discover_with(&session, &layout.providers(), with_tools()).unwrap();
    keep_candidate(&session, &id_of(&discovery, "downloads/d"));
    let discovery = discover_with(&session, &layout.providers(), with_tools()).unwrap();
    let wanted: Vec<String> = ["cache", "simulator-runtime", "downloads/d"]
        .into_iter()
        .map(|path| {
            let found = discovery
                .categories
                .iter()
                .flat_map(|c| &c.candidates)
                .find(|c| c.path.contains(path));
            found.unwrap().id.clone()
        })
        .collect();
    let preview = session
        .clean_preview_with_mode(wanted.clone(), FfiDeleteMode::Permanent)
        .unwrap();
    assert!(by_path(&preview, "cache").permanent);
    assert!(
        by_path(&preview, RUNTIME).permanent,
        "a runtime has no Trash route"
    );
    let cache_size = by_path(&preview, "cache").size_bytes;
    assert_eq!(
        preview.bytes_to_free, cache_size,
        "the runtime is not freed"
    );
    assert!(
        preview.bytes_to_free_is_upper_bound,
        "a prune frees at most that"
    );
    assert_eq!(preview.bytes_requested, RUNTIME_BYTES);
    assert_eq!(preview.bytes_to_trash, 0, "the kept file is excluded");
    let excluded: Vec<&str> = preview.excluded.iter().map(|e| e.path.as_str()).collect();
    assert_eq!(excluded.len(), 1);
    assert!(excluded[0].ends_with("downloads/d"), "never the runtime");
    let one = allocated(&layout.dir.join("cache/one"));

    let sinks = FakeTrash::default();
    let report = layout
        .remove(&session, &preview.preview_id, &sinks)
        .unwrap();
    assert_eq!(
        outcome(&report, "cache"),
        &FfiRemovalOutcome::Pruned { bytes_freed: one }
    );
    assert!(one < cache_size);
    assert_eq!(
        outcome(&report, RUNTIME),
        &FfiRemovalOutcome::RemovalRequested
    );
    assert_eq!(report.bytes_freed, one, "measured, never the size");
    assert_eq!(report.bytes_requested, RUNTIME_BYTES);
    assert_eq!(report.bytes_moved_to_trash, 0);
    assert_eq!(report.deleted_count, 1);
    assert_eq!(
        layout.log(),
        ["run cache", format!("run {RUNTIME}").as_str()]
    );
    assert!(sinks.steps().is_empty(), "no Trash move, no deletion");
    remove(&layout.dir);
}

#[test]
fn trash_mode_moves_a_prunable_cache_and_still_requests_a_runtime() {
    let layout = Layout::new("tools-trash");
    let session = TestSession::new();
    let discovery = discover_with(&session, &layout.providers(), with_tools()).unwrap();
    let preview = session
        .clean_preview_with_mode(ids(&discovery), FfiDeleteMode::Trash)
        .unwrap();
    assert!(!by_path(&preview, "cache").permanent);
    assert!(by_path(&preview, RUNTIME).permanent);
    assert_eq!(preview.bytes_to_free, 0);
    let sinks = FakeTrash::default();
    let report = layout
        .remove(&session, &preview.preview_id, &sinks)
        .unwrap();
    assert_eq!(outcome(&report, "cache"), &FfiRemovalOutcome::MovedToTrash);
    assert_eq!(
        outcome(&report, RUNTIME),
        &FfiRemovalOutcome::RemovalRequested
    );
    assert_eq!(layout.log(), [format!("run {RUNTIME}")]);
    assert_eq!(sinks.steps(), ["trash cache", "trash d"]);
    remove(&layout.dir);
}

#[test]
fn the_old_execute_refuses_a_preview_with_a_tool_command_unspent() {
    let layout = Layout::new("tools-old-execute");
    let session = TestSession::new();
    let discovery = discover_with(&session, &layout.providers(), with_tools()).unwrap();
    let preview = session
        .clean_preview(candidate_ids(&discovery, "simulator-runtimes"))
        .unwrap();
    let providers = |_: &CleanOptions| layout.providers();
    let refused = session.execute_clean(
        &preview.preview_id,
        &providers,
        &NotRunning,
        &FakeTrash::default(),
        &CancellationToken::default(),
        &Progress::default(),
    );
    assert!(matches!(refused, Err(FfiError::InvalidInput { .. })));
    assert!(layout.log().is_empty());
    let report = layout
        .remove(&session, &preview.preview_id, &FakeTrash::default())
        .unwrap();
    assert_eq!(
        report.results[0].outcome,
        FfiRemovalOutcome::RemovalRequested
    );
    remove(&layout.dir);
}

#[test]
fn a_failed_tool_is_never_retried_another_way() {
    let mut layout = Layout::new("tools-fail");
    layout.fail = true;
    let session = TestSession::new();
    let discovery = discover_with(&session, &layout.providers(), with_tools()).unwrap();
    let tools: Vec<String> = ["uv", "simulator-runtimes"]
        .into_iter()
        .flat_map(|id| candidate_ids(&discovery, id))
        .collect();
    let preview = session
        .clean_preview_with_mode(tools, FfiDeleteMode::Permanent)
        .unwrap();
    let sinks = FakeTrash::default();
    let report = layout
        .remove(&session, &preview.preview_id, &sinks)
        .unwrap();
    for result in &report.results {
        assert!(
            matches!(
                result.outcome,
                FfiRemovalOutcome::Failed {
                    reason: FfiRemovalFailure::ToolFailed,
                    ..
                }
            ),
            "{:?}",
            result.outcome
        );
    }
    assert_eq!(report.failed_count, 2);
    assert_eq!(layout.log().len(), 2, "one call each");
    assert!(
        sinks.steps().is_empty(),
        "no deletion, no Trash, no administrator rights"
    );
    assert_eq!((report.bytes_freed, report.bytes_requested), (0, 0));
    assert!(layout.dir.join("cache/one").exists());
    remove(&layout.dir);
}
