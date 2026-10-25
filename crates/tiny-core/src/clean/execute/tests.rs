use super::*;
use crate::clean::process::test_support::{FailingProbe, MockChecker};
use crate::clean::types::RiskLevel;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

struct CountingProvider {
    id: &'static str,
    calls: Arc<AtomicUsize>,
    last_len: Arc<AtomicUsize>,
}
impl CleanProvider for CountingProvider {
    fn id(&self) -> &'static str {
        self.id
    }
    fn label(&self) -> &'static str {
        "test"
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Safe
    }
    fn inclusion_reason(&self) -> String {
        "fixture".into()
    }
    fn discover(
        &self,
        _ctx: &crate::clean::scan_context::ScanContext<'_>,
    ) -> Result<Vec<CleanItem>> {
        Ok(Vec::new())
    }
    fn desktop_trash_paths(&self) -> bool {
        self.id == "vscode"
    }
    fn item_apps(&self, _: &std::path::Path) -> Vec<String> {
        if self.id == "vscode" {
            vec!["Code".into()]
        } else {
            vec![]
        }
    }
    fn check_item(&self, path: &std::path::Path) -> std::result::Result<(), String> {
        if path.ends_with("denied") {
            Err("provider refused fixture".into())
        } else {
            Ok(())
        }
    }
    fn execute(&self, items: &[CleanItem], _action: ExecAction) -> Result<ExecReport> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        self.last_len.store(items.len(), Ordering::SeqCst);
        Ok(ExecReport {
            removed_paths: items
                .iter()
                .filter(|i| !i.path.ends_with("failure"))
                .map(|i| i.path.clone())
                .collect(),
            failed: items
                .iter()
                .filter(|i| i.path.ends_with("failure"))
                .map(|i| (i.path.clone(), "fixture mutation failed".into()))
                .collect(),
            ..ExecReport::default()
        })
    }
}

fn item(path: &str, cat: &str) -> CleanItem {
    CleanItem {
        category_id: cat.into(),
        category_label: cat.into(),
        path: PathBuf::from(path),
        size: 0,
        risk: RiskLevel::Safe,
        evidence: Vec::new(),
    }
}

fn group_with(items: Vec<CleanItem>, id: &str) -> CategoryGroup {
    CategoryGroup {
        id: id.into(),
        label: id.into(),
        risk: RiskLevel::Safe,
        total_size: 0,
        items,
    }
}

#[test]
fn drill_down_filter_removes_empty_categories() {
    let calls = Arc::new(AtomicUsize::new(0));
    let last_len = Arc::new(AtomicUsize::new(0));
    let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(CountingProvider {
        id: "user-logs",
        calls: calls.clone(),
        last_len: last_len.clone(),
    })];
    let g = group_with(
        vec![item("/a", "user-logs"), item("/b", "user-logs")],
        "user-logs",
    );
    let groups = vec![&g];
    let mut excluded = HashSet::new();
    excluded.insert(PathBuf::from("/a"));
    excluded.insert(PathBuf::from("/b"));
    let report = execute_with_providers(
        &groups,
        CleanAction::Trash,
        &excluded,
        &PolicySnapshot::absent(Vec::new()),
        &providers,
        &MockChecker::none(),
    )
    .unwrap();
    assert_eq!(report.removed_paths.len(), 0);
    assert_eq!(
        calls.load(Ordering::SeqCst),
        0,
        "provider must NOT be called when filtered set is empty"
    );
}

#[test]
fn execute_filters_only_excluded_items() {
    let calls = Arc::new(AtomicUsize::new(0));
    let last_len = Arc::new(AtomicUsize::new(0));
    let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(CountingProvider {
        id: "user-logs",
        calls: calls.clone(),
        last_len: last_len.clone(),
    })];
    let g = group_with(
        vec![
            item("/a", "user-logs"),
            item("/b", "user-logs"),
            item("/c", "user-logs"),
        ],
        "user-logs",
    );
    let groups = vec![&g];
    let mut excluded = HashSet::new();
    excluded.insert(PathBuf::from("/b"));
    let _ = execute_with_providers(
        &groups,
        CleanAction::Trash,
        &excluded,
        &PolicySnapshot::absent(Vec::new()),
        &providers,
        &MockChecker::none(),
    )
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        last_len.load(Ordering::SeqCst),
        2,
        "filtered should pass 2 items, not 3"
    );
}

#[test]
fn always_keep_rules_hold_items_before_any_provider_runs() {
    use crate::clean::protection::{ProtectionRule, RuleScope, RulesDocument};
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let (kept, free) = (root.join("kept"), root.join("free"));
    std::fs::write(&kept, b"k").unwrap();
    std::fs::write(&free, b"f").unwrap();
    let policy = PolicySnapshot::from_document(
        RulesDocument {
            version: 1,
            revision: 1,
            next_id: 2,
            rules: vec![ProtectionRule {
                id: "r1".into(),
                path: kept.clone(),
                scope: RuleScope::File,
            }],
        },
        Vec::new(),
    );
    let calls = Arc::new(AtomicUsize::new(0));
    let last_len = Arc::new(AtomicUsize::new(0));
    let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(CountingProvider {
        id: "user-logs",
        calls: calls.clone(),
        last_len: last_len.clone(),
    })];
    let group = group_with(
        vec![
            item(kept.to_str().unwrap(), "user-logs"),
            item(free.to_str().unwrap(), "user-logs"),
        ],
        "user-logs",
    );
    let report = execute_with_providers(
        &[&group],
        CleanAction::HardDelete,
        &HashSet::new(),
        &policy,
        &providers,
        &MockChecker::none(),
    )
    .unwrap();
    assert_eq!(
        calls.load(Ordering::SeqCst),
        1,
        "only the free item reaches the provider"
    );
    assert_eq!(last_len.load(Ordering::SeqCst), 1);
    assert!(report
        .failed
        .iter()
        .any(|(path, reason)| path == &kept && reason.contains("Always keep")));
}

#[test]
fn excluded_descendant_blocks_ancestor_for_both_actions() {
    for action in [CleanAction::Trash, CleanAction::HardDelete] {
        let calls = Arc::new(AtomicUsize::new(0));
        let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(CountingProvider {
            id: "user-logs",
            calls: calls.clone(),
            last_len: Arc::new(AtomicUsize::new(0)),
        })];
        let parent = group_with(vec![item("/logs/JetBrains", "user-logs")], "user-logs");
        let child = group_with(
            vec![item("/logs/JetBrains/Product", "jetbrains")],
            "jetbrains",
        );
        let excluded = HashSet::from([PathBuf::from("/logs/JetBrains/Product")]);
        let report = execute_with_providers(
            &[&parent, &child],
            action,
            &excluded,
            &PolicySnapshot::absent(Vec::new()),
            &providers,
            &MockChecker::none(),
        )
        .unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "excluded child must block its selected ancestor"
        );
        assert_eq!(
            report.failed.len(),
            1,
            "explain why the selected parent was kept"
        );
        assert_eq!(report.failed[0].0, PathBuf::from("/logs/JetBrains"));
    }
}

#[test]
fn exclusions_keep_nested_items_but_not_similar_sibling_names() {
    let calls = Arc::new(AtomicUsize::new(0));
    let last_len = Arc::new(AtomicUsize::new(0));
    let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(CountingProvider {
        id: "user-logs",
        calls: calls.clone(),
        last_len: last_len.clone(),
    })];
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    let sibling = root.join("keeper");
    let child = root.join("keep/child");
    let group = group_with(
        vec![
            item(child.to_str().unwrap(), "user-logs"),
            item(sibling.to_str().unwrap(), "user-logs"),
        ],
        "user-logs",
    );
    let excluded = HashSet::from([root.join("keep")]);
    let report = execute_with_providers(
        &[&group],
        CleanAction::Trash,
        &excluded,
        &PolicySnapshot::absent(Vec::new()),
        &providers,
        &MockChecker::none(),
    )
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(
        last_len.load(Ordering::SeqCst),
        1,
        "only the unrelated sibling reaches the provider"
    );
    assert_eq!(report.failed[0].0, child);
}

#[test]
fn hard_delete_maps_to_empty_trash_for_trash_provider() {
    assert_eq!(
        map_action(CleanAction::HardDelete, "trash").unwrap(),
        ExecAction::EmptyTrash
    );
}

#[test]
fn execution_refuses_running_apps_failed_probes_and_provider_guards() {
    let running = MockChecker::with_running(["Code"]);
    let absent = MockChecker::none();
    for (probe, path) in [
        (&running as &dyn AppProbe, "/cache"),
        (&FailingProbe, "/cache"),
        (&absent, "/denied"),
    ] {
        let calls = Arc::new(AtomicUsize::new(0));
        let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(CountingProvider {
            id: "vscode",
            calls: calls.clone(),
            last_len: Arc::new(AtomicUsize::new(0)),
        })];
        let group = group_with(vec![item(path, "vscode")], "vscode");
        let report = execute_with_providers(
            &[&group],
            CleanAction::Trash,
            &HashSet::new(),
            &PolicySnapshot::absent(Vec::new()),
            &providers,
            probe,
        )
        .unwrap();
        assert_eq!(
            calls.load(Ordering::SeqCst),
            0,
            "unsafe item must not reach the provider"
        );
        assert_eq!(report.failed.len(), 1);
    }
}

#[test]
fn app_is_rechecked_between_individual_path_mutations() {
    struct OpensAfterFirst(Arc<AtomicUsize>);
    impl AppProbe for OpensAfterFirst {
        fn probe(&self, _: &str) -> std::result::Result<bool, String> {
            Ok(self.0.load(Ordering::SeqCst) > 0)
        }
    }
    let calls = Arc::new(AtomicUsize::new(0));
    let last_len = Arc::new(AtomicUsize::new(0));
    let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(CountingProvider {
        id: "vscode",
        calls: calls.clone(),
        last_len: last_len.clone(),
    })];
    let group = group_with(
        vec![item("/first", "vscode"), item("/second", "vscode")],
        "vscode",
    );
    let report = execute_with_providers(
        &[&group],
        CleanAction::HardDelete,
        &HashSet::new(),
        &PolicySnapshot::absent(Vec::new()),
        &providers,
        &OpensAfterFirst(calls.clone()),
    )
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(last_len.load(Ordering::SeqCst), 1);
    assert_eq!(report.failed[0].0, PathBuf::from("/second"));
}

#[test]
fn per_path_execution_preserves_success_guard_refusal_and_provider_failure() {
    let calls = Arc::new(AtomicUsize::new(0));
    let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(CountingProvider {
        id: "vscode",
        calls: calls.clone(),
        last_len: Arc::new(AtomicUsize::new(0)),
    })];
    let group = group_with(
        ["/first", "/denied", "/failure"]
            .map(|p| item(p, "vscode"))
            .to_vec(),
        "vscode",
    );
    let report = execute_with_providers(
        &[&group],
        CleanAction::Trash,
        &HashSet::new(),
        &PolicySnapshot::absent(Vec::new()),
        &providers,
        &MockChecker::none(),
    )
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 2);
    assert_eq!(report.removed_paths, vec![PathBuf::from("/first")]);
    assert_eq!(
        report.failed,
        vec![
            (
                PathBuf::from("/denied"),
                "kept: provider refused fixture".into()
            ),
            (PathBuf::from("/failure"), "fixture mutation failed".into()),
        ]
    );
}

/// A category that needs administrator rights; counts `execute` calls.
struct AdminProvider {
    calls: Arc<AtomicUsize>,
}
impl CleanProvider for AdminProvider {
    fn id(&self) -> &'static str {
        "system-simulator-caches"
    }
    fn label(&self) -> &'static str {
        "test"
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Safe
    }
    fn inclusion_reason(&self) -> String {
        "fixture".into()
    }
    fn discover(
        &self,
        _ctx: &crate::clean::scan_context::ScanContext<'_>,
    ) -> Result<Vec<CleanItem>> {
        Ok(Vec::new())
    }
    fn needs_admin(&self) -> bool {
        true
    }
    fn desktop_trash_paths(&self) -> bool {
        true
    }
    fn execute(&self, items: &[CleanItem], _action: ExecAction) -> Result<ExecReport> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(ExecReport {
            removed_paths: items.iter().map(|i| i.path.clone()).collect(),
            ..ExecReport::default()
        })
    }
}

#[test]
fn the_cli_refuses_categories_that_need_admin_before_the_provider_runs() {
    let calls = Arc::new(AtomicUsize::new(0));
    let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(AdminProvider {
        calls: calls.clone(),
    })];
    let paths = ["/Library/Developer/CoreSimulator/Caches/tiny-test-missing"];
    let group = group_with(
        paths.map(|p| item(p, "system-simulator-caches")).to_vec(),
        "system-simulator-caches",
    );
    for action in [CleanAction::Trash, CleanAction::HardDelete] {
        let report = execute_with_providers(
            &[&group],
            action,
            &HashSet::new(),
            &PolicySnapshot::absent(Vec::new()),
            &providers,
            &FailingProbe,
        )
        .unwrap();
        assert!(report.removed_paths.is_empty());
        assert_eq!(
            report.failed,
            vec![(
                PathBuf::from(paths[0]),
                format!("kept: {}", crate::clean::providers::NEEDS_ADMIN)
            )]
        );
    }
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

/// Every registered category except `trash`, whose hard delete empties
/// the real Trash whatever the paths. The paths do not exist, so nothing
/// on disk could change even if a check were missing.
#[test]
fn no_cli_execute_path_accepts_a_path_inside_system() {
    use crate::clean::providers::{all_providers_with, known_category_ids};
    use crate::runner::test_support::MockRunner;
    let runner: Arc<dyn crate::runner::CommandRunner> = Arc::new(MockRunner::new());
    let providers = all_providers_with(&CleanOpts::default(), runner);
    let paths = [
        "/System/Library/AssetsV2/tiny-test-missing/x.dmg",
        "/system/library/assetsv2/tiny-test-missing/x.dmg",
        "/System/Volumes/Data/System/Library/AssetsV2/tiny-test-missing/x.dmg",
        "/Library/Developer/CoreSimulator/Caches/../../../../System/tiny-test-missing",
    ];
    let groups: Vec<CategoryGroup> = known_category_ids()
        .iter()
        .filter(|id| **id != TRASH_ID)
        .map(|id| group_with(paths.map(|p| item(p, id)).to_vec(), id))
        .collect();
    let refs: Vec<&CategoryGroup> = groups.iter().collect();
    let report = execute_with_providers(
        &refs,
        CleanAction::HardDelete,
        &HashSet::new(),
        &PolicySnapshot::absent(Vec::new()),
        &providers,
        &MockChecker::none(),
    )
    .unwrap();
    assert!(
        report.removed_paths.is_empty(),
        "{:?}",
        report.removed_paths
    );
    assert!(report.removal_requested.is_empty());
    assert_eq!(report.failed.len(), groups.len() * paths.len());
}

#[test]
fn hard_delete_maps_to_hard_delete_for_others() {
    assert_eq!(
        map_action(CleanAction::HardDelete, "user-logs").unwrap(),
        ExecAction::HardDelete
    );
    assert_eq!(
        map_action(CleanAction::HardDelete, "cargo").unwrap(),
        ExecAction::HardDelete
    );
}

#[test]
fn trash_action_rejected_for_trash_provider() {
    assert!(map_action(CleanAction::Trash, "trash").is_err());
}

#[test]
fn trash_action_passes_for_others() {
    assert_eq!(
        map_action(CleanAction::Trash, "user-logs").unwrap(),
        ExecAction::Trash
    );
}

#[test]
fn symlinked_ancestor_never_reaches_the_provider_in_either_mode() {
    let directory = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(directory.path()).unwrap();
    std::fs::create_dir(root.join("real")).unwrap();
    std::os::unix::fs::symlink(root.join("real"), root.join("link")).unwrap();
    for action in [CleanAction::Trash, CleanAction::HardDelete] {
        let calls = Arc::new(AtomicUsize::new(0));
        let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(CountingProvider {
            id: "user-logs",
            calls: calls.clone(),
            last_len: Arc::new(AtomicUsize::new(0)),
        })];
        let group = group_with(
            vec![item(root.join("link/file").to_str().unwrap(), "user-logs")],
            "user-logs",
        );
        let report = execute_with_providers(
            &[&group],
            action,
            &HashSet::new(),
            &PolicySnapshot::absent(Vec::new()),
            &providers,
            &MockChecker::none(),
        )
        .unwrap();
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        assert_eq!(report.failed.len(), 1);
    }
}

#[test]
fn relative_files_cannot_impersonate_a_tool_identifier() {
    let calls = Arc::new(AtomicUsize::new(0));
    let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(CountingProvider {
        id: "user-logs",
        calls: calls.clone(),
        last_len: Arc::new(AtomicUsize::new(0)),
    })];
    let group = group_with(vec![item("relative/file", "user-logs")], "user-logs");
    let report = execute_with_providers(
        &[&group],
        CleanAction::Trash,
        &HashSet::new(),
        &PolicySnapshot::absent(Vec::new()),
        &providers,
        &MockChecker::none(),
    )
    .unwrap();
    assert!(report.removed_paths.is_empty());
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

#[test]
fn provider_owned_tool_ids_pass_policy_without_becoming_filesystem_paths() {
    use crate::clean::providers::docker::Docker;
    use crate::runner::test_support::MockRunner;
    let runner =
        Arc::new(MockRunner::new().with_response("docker", &["image", "prune", "-af"], true, ""));
    let providers: Vec<Box<dyn CleanProvider>> =
        vec![Box::new(Docker::with_runner(runner.clone()))];
    let group = group_with(vec![item("<docker:images>", "docker")], "docker");
    let report = execute_with_providers(
        &[&group],
        CleanAction::Trash,
        &HashSet::new(),
        &PolicySnapshot::absent(Vec::new()),
        &providers,
        &MockChecker::none(),
    )
    .unwrap();
    assert_eq!(report.removed_paths, [PathBuf::from("<docker:images>")]);
    assert_eq!(
        *runner.run_calls.lock().unwrap(),
        ["docker image prune -af"]
    );
}

#[test]
fn changed_policy_keeps_completed_outcomes_and_stops_later_mutations() {
    let calls = Arc::new(AtomicUsize::new(0));
    let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(CountingProvider {
        id: "vscode",
        calls: calls.clone(),
        last_len: Arc::new(AtomicUsize::new(0)),
    })];
    let group = group_with(vec![item("/a", "vscode"), item("/b", "vscode")], "vscode");
    let readings = AtomicUsize::new(0);
    let report = execute_with_policy(
        &[&group],
        CleanAction::Trash,
        &HashSet::new(),
        &PolicySnapshot::absent(Vec::new()),
        &providers,
        &MockChecker::none(),
        &|| {
            if readings.fetch_add(1, Ordering::SeqCst) >= 3 {
                Err(engine_error!("policy changed"))
            } else {
                Ok(())
            }
        },
    )
    .unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert_eq!(report.removed_paths, [PathBuf::from("/a")]);
    assert_eq!(report.failed.len(), 1);
    assert_eq!(report.failed[0].0, PathBuf::from("/b"));
}
