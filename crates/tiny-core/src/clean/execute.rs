//! Execute a plan against per-provider executors.
//!
//! Translates the user-facing `CleanAction` into provider-specific
//! `ExecAction`s:
//! - `CleanAction::Trash` → `ExecAction::Trash` for every provider.
//!   Trash provider rejects this; we therefore refuse `Trash` whenever a
//!   `trash` group is in the plan (the user must pick Hard delete instead).
//! - `CleanAction::HardDelete` → `ExecAction::EmptyTrash` for the Trash
//!   provider, `ExecAction::HardDelete` for everyone else.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::engine_error;
use crate::error::Result;

use crate::options::CleanOptions as CleanOpts;

use super::discover::CategoryGroup;
use super::process::{AppProbe, RunnerProbe};
use super::protection::{Matcher, PolicySnapshot};
use super::protection_store::ProtectionStore;
use super::providers::{all_providers, CleanProvider};
use super::types::{CleanAction, CleanItem, ExecAction, ExecReport};
use crate::runner::RealRunner;

const TRASH_ID: &str = "trash";

pub fn execute(
    groups: &[&CategoryGroup],
    action: CleanAction,
    excluded_paths: &HashSet<PathBuf>,
    opts: &CleanOpts,
) -> Result<ExecReport> {
    let has_trash = groups.iter().any(|g| g.id == TRASH_ID);
    if has_trash && matches!(action, CleanAction::Trash) {
        return Err(engine_error!(
            "trash category requires --hard (cannot 'move trash to trash')"
        ));
    }

    // Always keep rules are saved by the desktop app; unreadable rules stop the run.
    let policy = ProtectionStore::for_account()
        .and_then(|store| store.load())
        .map_err(|e| {
            engine_error!(
                "cannot read the kept paths saved by the TinySift app, so nothing was cleaned: {e}"
            )
        })?;
    let providers = all_providers(opts);
    execute_with_providers(
        groups,
        action,
        excluded_paths,
        &policy,
        &providers,
        &RunnerProbe(&RealRunner),
    )
}

/// Same as `execute` but takes the provider list as a parameter so tests
/// can inject mocks. The public entry resolves providers via
/// `all_providers(opts)` and delegates here.
pub(crate) fn execute_with_providers(
    groups: &[&CategoryGroup],
    action: CleanAction,
    excluded_paths: &HashSet<PathBuf>,
    policy: &PolicySnapshot,
    providers: &[Box<dyn CleanProvider>],
    probe: &dyn AppProbe,
) -> Result<ExecReport> {
    let mut report = ExecReport::default();
    let excluded_ancestors: HashSet<_> = excluded_paths
        .iter()
        .flat_map(|path| path.ancestors())
        .collect();
    for group in groups {
        let filtered = reviewed_items(
            &group.items,
            excluded_paths,
            &excluded_ancestors,
            policy,
            &mut report,
        );
        if filtered.is_empty() {
            continue;
        }
        let provider = providers
            .iter()
            .find(|p| p.id() == group.id)
            .ok_or_else(|| {
                engine_error!(
                    "internal error: no provider registered for category '{}'",
                    group.id
                )
            })?;
        let exec_action = map_action(action, group.id.as_str())?;
        report.merge(execute_group(
            provider.as_ref(),
            &filtered,
            exec_action,
            probe,
        )?);
    }
    Ok(report)
}

fn execute_group(
    provider: &dyn CleanProvider,
    items: &[CleanItem],
    action: ExecAction,
    probe: &dyn AppProbe,
) -> Result<ExecReport> {
    // Recheck file providers immediately before each path; tool-backed providers
    // retain their batch semantics (for example one Docker prune).
    if !provider.desktop_trash_paths() {
        return execute_guarded(provider, items, action, probe);
    }
    let mut report = ExecReport::default();
    for item in items {
        report.merge(execute_guarded(
            provider,
            std::slice::from_ref(item),
            action,
            probe,
        )?);
    }
    Ok(report)
}

fn reviewed_items(
    items: &[CleanItem],
    excluded: &HashSet<PathBuf>,
    ancestors: &HashSet<&std::path::Path>,
    policy: &PolicySnapshot,
    report: &mut ExecReport,
) -> Vec<CleanItem> {
    let matcher = Matcher::new();
    items
        .iter()
        .filter(|item| !excluded.contains(&item.path))
        .filter(|item| {
            let reason = if ancestors.contains(item.path.as_path())
                || item.path.ancestors().any(|path| excluded.contains(path))
            {
                "kept: overlaps a path excluded during review"
            } else {
                let evaluation = policy.evaluate(&matcher, &item.path);
                if evaluation.allows_move() {
                    return true;
                }
                if evaluation.blocks.is_empty() {
                    "kept: cannot confirm that no Always keep rule covers it"
                } else {
                    "kept: an Always keep rule in the TinySift app covers it"
                }
            };
            report.failed.push((item.path.clone(), reason.into()));
            false
        })
        .cloned()
        .collect()
}

fn execute_guarded(
    provider: &dyn CleanProvider,
    items: &[CleanItem],
    action: ExecAction,
    probe: &dyn AppProbe,
) -> Result<ExecReport> {
    let mut report = ExecReport::default();
    let allowed: Vec<_> = items
        .iter()
        .filter(|item| match check_item(provider, item, probe) {
            Ok(()) => true,
            Err(reason) => {
                report
                    .failed
                    .push((item.path.clone(), format!("kept: {reason}")));
                false
            }
        })
        .cloned()
        .collect();
    if !allowed.is_empty() {
        report.merge(provider.execute(&allowed, action)?);
    }
    Ok(report)
}

fn check_item(
    provider: &dyn CleanProvider,
    item: &CleanItem,
    probe: &dyn AppProbe,
) -> std::result::Result<(), String> {
    for app in provider.item_apps(&item.path) {
        match probe.probe(&app) {
            Ok(false) => {}
            Ok(true) => return Err(format!("{app} is running")),
            Err(error) => return Err(format!("running-app check for {app} failed: {error}")),
        }
    }
    provider.check_item(&item.path)
}

/// Map a UI action to per-provider `ExecAction`. Visible to tests.
pub fn map_action(action: CleanAction, provider_id: &str) -> Result<ExecAction> {
    match action {
        CleanAction::Trash => {
            if provider_id == TRASH_ID {
                Err(engine_error!("trash provider rejects Trash action"))
            } else {
                Ok(ExecAction::Trash)
            }
        }
        CleanAction::HardDelete => {
            if provider_id == TRASH_ID {
                Ok(ExecAction::EmptyTrash)
            } else {
                Ok(ExecAction::HardDelete)
            }
        }
        CleanAction::DryRun | CleanAction::Cancel => Err(engine_error!(
            "DryRun/Cancel must be handled before execute()"
        )),
    }
}

#[allow(dead_code)]
fn ensure_provider_exists(providers: &[Box<dyn CleanProvider>], id: &str) -> Result<()> {
    if providers.iter().any(|p| p.id() == id) {
        Ok(())
    } else {
        Err(engine_error!("no provider for '{}'", id))
    }
}

#[cfg(test)]
mod tests {
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
        let group = group_with(
            vec![
                item("/logs/keep/child", "user-logs"),
                item("/logs/keeper", "user-logs"),
            ],
            "user-logs",
        );
        let excluded = HashSet::from([PathBuf::from("/logs/keep")]);
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
        assert_eq!(report.failed[0].0, PathBuf::from("/logs/keep/child"));
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
}
