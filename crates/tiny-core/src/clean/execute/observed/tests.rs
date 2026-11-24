use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use super::super::{execute_with_policy, ReviewExclusion};
use super::*;
use crate::clean::process::test_support::MockChecker;
use crate::clean::protection_store::StoreError;
use crate::clean::types::{CleanItem, ExecAction, RiskLevel};

/// Removes every item except those named `failure`, counting its calls;
/// an item named `explodes` makes the whole call fail.
struct Recording {
    id: &'static str,
    calls: Arc<AtomicUsize>,
}

impl CleanProvider for Recording {
    fn id(&self) -> &'static str {
        self.id
    }
    fn label(&self) -> &'static str {
        "fixture"
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
    fn check_item(&self, path: &std::path::Path) -> std::result::Result<(), String> {
        if path.ends_with("denied") {
            Err("provider refused fixture".into())
        } else {
            Ok(())
        }
    }
    fn execute(&self, items: &[CleanItem], _action: ExecAction) -> Result<ExecReport> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if items.iter().any(|i| i.path.ends_with("explodes")) {
            return Err(crate::engine_error!("fixture tool crashed"));
        }
        let (failed, removed): (Vec<_>, Vec<_>) =
            items.iter().partition(|i| i.path.ends_with("failure"));
        Ok(ExecReport {
            removed_paths: removed.iter().map(|i| i.path.clone()).collect(),
            failed: failed
                .iter()
                .map(|i| (i.path.clone(), "fixture mutation failed".into()))
                .collect(),
            ..ExecReport::default()
        })
    }
}

fn group(id: &str, paths: &[&str]) -> CategoryGroup {
    CategoryGroup {
        id: id.into(),
        label: id.into(),
        risk: RiskLevel::Safe,
        total_size: 0,
        items: paths
            .iter()
            .map(|path| CleanItem {
                category_id: id.into(),
                category_label: id.into(),
                path: PathBuf::from(path),
                size: 0,
                risk: RiskLevel::Safe,
                evidence: Vec::new(),
            })
            .collect(),
    }
}

const IDS: [&str; 3] = ["user-logs", "crash-reports", "xcode-derived"];

fn providers(calls: &[Arc<AtomicUsize>; 3]) -> Vec<Box<dyn CleanProvider>> {
    IDS.iter()
        .zip(calls)
        .map(|(id, calls)| {
            Box::new(Recording {
                id,
                calls: calls.clone(),
            }) as Box<dyn CleanProvider>
        })
        .collect()
}

fn fixture_groups() -> Vec<CategoryGroup> {
    vec![
        group(IDS[0], &["/a", "/failure", "/excluded"]),
        group(IDS[1], &["/c/denied", "/d"]),
        group(IDS[2], &["/e"]),
    ]
}

fn counters() -> [Arc<AtomicUsize>; 3] {
    std::array::from_fn(|_| Arc::new(AtomicUsize::new(0)))
}

fn summary(report: &ExecReport) -> (Vec<PathBuf>, Vec<(PathBuf, String)>, Vec<PathBuf>) {
    (
        report.removed_paths.clone(),
        report.failed.clone(),
        report.removal_requested.clone(),
    )
}

fn paths(list: &[&str]) -> Vec<PathBuf> {
    list.iter().map(PathBuf::from).collect()
}

#[test]
fn each_group_is_observed_with_what_it_did_and_execute_returns_the_same() {
    let groups = fixture_groups();
    let refs: Vec<&CategoryGroup> = groups.iter().collect();
    let excluded = HashSet::from([PathBuf::from("/excluded")]);
    let policy = PolicySnapshot::absent(Vec::new());
    let mut seen = Vec::new();
    let observed = observed_with_policy(
        &refs,
        CleanAction::Trash,
        &excluded,
        &policy,
        &providers(&counters()),
        &MockChecker::none(),
        &|| Ok(()),
        &mut |outcome| {
            seen.push(outcome.clone());
            ControlFlow::Continue(())
        },
    )
    .unwrap();

    let per_group: Vec<_> = seen
        .iter()
        .map(|o| {
            (
                o.category_id.as_str(),
                summary(&o.report),
                o.stopped.clone(),
            )
        })
        .collect();
    let failed = |path: &str, reason: &str| (PathBuf::from(path), reason.to_string());
    assert_eq!(
        per_group,
        [
            (
                IDS[0],
                (
                    paths(&["/a"]),
                    vec![failed("/failure", "fixture mutation failed")],
                    vec![]
                ),
                None
            ),
            (
                IDS[1],
                (
                    paths(&["/d"]),
                    vec![failed("/c/denied", "kept: provider refused fixture")],
                    vec![]
                ),
                None
            ),
            (IDS[2], (paths(&["/e"]), vec![], vec![]), None),
        ]
    );
    assert!(observed.not_run.is_empty());
    let expected = (
        paths(&["/a", "/d", "/e"]),
        vec![
            failed("/failure", "fixture mutation failed"),
            failed("/c/denied", "kept: provider refused fixture"),
        ],
        vec![],
    );
    assert_eq!(summary(&observed.report), expected);
    let through_execute = execute_with_policy(
        &refs,
        CleanAction::Trash,
        &excluded,
        &policy,
        &providers(&counters()),
        &MockChecker::none(),
        &|| Ok(()),
    )
    .unwrap();
    assert_eq!(summary(&through_execute), expected);
}

#[test]
fn a_group_that_errors_is_observed_as_stopped_and_the_run_returns_its_error() {
    let groups = [
        group(IDS[0], &["/a"]),
        group(IDS[1], &["/d", "/explodes"]),
        group(IDS[2], &["/e"]),
    ];
    let refs: Vec<&CategoryGroup> = groups.iter().collect();
    let policy = PolicySnapshot::absent(Vec::new());
    let calls = counters();
    let mut seen = Vec::new();
    let result = observed_with_policy(
        &refs,
        CleanAction::Trash,
        &HashSet::new(),
        &policy,
        &providers(&calls),
        &MockChecker::none(),
        &|| Ok(()),
        &mut |outcome| {
            seen.push((outcome.category_id.clone(), outcome.stopped.clone()));
            ControlFlow::Continue(())
        },
    );
    let error = result.unwrap_err().to_string();
    assert_eq!(error, "fixture tool crashed");
    assert_eq!(
        seen,
        [
            (IDS[0].to_string(), None),
            (IDS[1].to_string(), Some("fixture tool crashed".to_string())),
        ]
    );
    assert_eq!(calls[2].load(Ordering::SeqCst), 0, "nothing ran after it");
    // `execute` still fails with the same error.
    let through_execute = execute_with_policy(
        &refs,
        CleanAction::Trash,
        &HashSet::new(),
        &policy,
        &providers(&counters()),
        &MockChecker::none(),
        &|| Ok(()),
    );
    assert_eq!(through_execute.unwrap_err().to_string(), error);
}

#[test]
fn the_review_rule_is_what_the_run_keeps() {
    // A path of one category unticked inside an item of another.
    let groups = [
        group(IDS[0], &["/x/cache", "/y"]),
        group(IDS[1], &["/x/cache/sub/file", "/z"]),
        group(IDS[2], &["/x"]),
    ];
    let refs: Vec<&CategoryGroup> = groups.iter().collect();
    let excluded = HashSet::from([PathBuf::from("/x/cache/sub/file")]);
    let run = observed_with_policy(
        &refs,
        CleanAction::Trash,
        &excluded,
        &PolicySnapshot::absent(Vec::new()),
        &providers(&counters()),
        &MockChecker::none(),
        &|| Ok(()),
        &mut |_| ControlFlow::Continue(()),
    )
    .unwrap();
    let rule = ReviewExclusion::new(&excluded);
    let handed: Vec<PathBuf> = groups
        .iter()
        .flat_map(|g| &g.items)
        .filter(|item| !rule.keeps(&item.path))
        .map(|item| item.path.clone())
        .collect();
    assert_eq!(handed, paths(&["/y", "/z"]));
    assert_eq!(run.report.removed_paths, handed);
}

#[test]
fn break_stops_before_the_next_group_and_reports_it_not_run() {
    let groups = fixture_groups();
    let refs: Vec<&CategoryGroup> = groups.iter().collect();
    let calls = counters();
    let mut observed_ids = Vec::new();
    let run = observed_with_policy(
        &refs,
        CleanAction::HardDelete,
        &HashSet::new(),
        &PolicySnapshot::absent(Vec::new()),
        &providers(&calls),
        &MockChecker::none(),
        &|| Ok(()),
        &mut |outcome| {
            observed_ids.push(outcome.category_id.clone());
            ControlFlow::Break(())
        },
    )
    .unwrap();

    assert_eq!(observed_ids, [IDS[0]]);
    assert_eq!(
        calls[0].load(Ordering::SeqCst),
        1,
        "the first group finished"
    );
    assert_eq!(
        calls[1].load(Ordering::SeqCst),
        0,
        "nothing moved after Break"
    );
    assert_eq!(
        calls[2].load(Ordering::SeqCst),
        0,
        "nothing moved after Break"
    );
    assert_eq!(run.not_run, [IDS[1], IDS[2]]);
    assert_eq!(
        run.report.removed_paths,
        [PathBuf::from("/a"), PathBuf::from("/excluded")]
    );
    let not_run_paths = ["/c/denied", "/d", "/e"].map(PathBuf::from);
    assert!(
        run.report
            .failed
            .iter()
            .all(|(path, _)| !not_run_paths.contains(path)),
        "groups that did not run are not reported as failed: {:?}",
        run.report.failed
    );
}

#[test]
fn one_lease_is_held_from_the_first_group_to_the_last() {
    let home = tempfile::tempdir().unwrap();
    let store = ProtectionStore::at(home.path().join("Tiny").join("clean-protection"));
    let groups = fixture_groups();
    let refs: Vec<&CategoryGroup> = groups.iter().collect();
    let mut busy_between_groups = Vec::new();
    let run = execute_observed_in(
        &store,
        &refs,
        CleanAction::Trash,
        &HashSet::new(),
        &providers(&counters()),
        &MockChecker::none(),
        &mut |_| {
            let other = ProtectionStore::at(store.dir().to_path_buf());
            busy_between_groups.push(matches!(other.lease(), Err(StoreError::Busy)));
            ControlFlow::Continue(())
        },
    )
    .unwrap();

    assert_eq!(busy_between_groups, [true, true, true]);
    assert_eq!(run.report.removed_paths.len(), 4);
    let after = ProtectionStore::at(store.dir().to_path_buf());
    assert!(after.lease().is_ok(), "the lease ends with the run");
}

#[test]
fn a_busy_store_refuses_the_run_before_any_group() {
    let home = tempfile::tempdir().unwrap();
    let store = ProtectionStore::at(home.path().join("Tiny").join("clean-protection"));
    let held = ProtectionStore::at(store.dir().to_path_buf());
    let _lease = held.lease().unwrap();
    let calls = counters();
    let groups = fixture_groups();
    let refs: Vec<&CategoryGroup> = groups.iter().collect();
    let mut observed = 0;
    let result = execute_observed_in(
        &store,
        &refs,
        CleanAction::Trash,
        &HashSet::new(),
        &providers(&calls),
        &MockChecker::none(),
        &mut |_| {
            observed += 1;
            ControlFlow::Continue(())
        },
    );
    assert!(result.is_err());
    assert_eq!(observed, 0);
    assert!(calls.iter().all(|c| c.load(Ordering::SeqCst) == 0));
}
