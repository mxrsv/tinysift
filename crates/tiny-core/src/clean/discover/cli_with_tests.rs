//! `discover_for_cli_with`: the CLI's checked discovery plus progress and cancellation.

use std::sync::atomic::Ordering;
use std::sync::Mutex;

use super::*;
use crate::clean::process::test_support::MockChecker;
use crate::clean::types::{ExecAction, ExecReport};
use crate::options::CleanOptions as CleanOpts;

/// Lists fixed items without touching the file system; `fail` errors instead.
struct Listed {
    id: &'static str,
    sizes: &'static [u64],
    fail: bool,
}

impl CleanProvider for Listed {
    fn id(&self) -> &'static str {
        self.id
    }
    fn label(&self) -> &'static str {
        self.id
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Safe
    }
    fn inclusion_reason(&self) -> String {
        "fixture".into()
    }
    fn discover(&self, _ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        if self.fail {
            return Err(crate::engine_error!("fixture failure"));
        }
        Ok(self
            .sizes
            .iter()
            .enumerate()
            .map(|(n, &size)| CleanItem {
                category_id: self.id.into(),
                category_label: self.id.into(),
                path: PathBuf::from(format!("/fixture/{}/{n}", self.id)),
                size,
                risk: RiskLevel::Safe,
                evidence: Vec::new(),
            })
            .collect())
    }
    fn execute(&self, _: &[CleanItem], _: ExecAction) -> Result<ExecReport> {
        unreachable!("discovery tests never execute")
    }
}

fn providers(fail_second: bool) -> Vec<Box<dyn CleanProvider>> {
    vec![
        Box::new(Listed {
            id: "first",
            sizes: &[5, 7],
            fail: false,
        }),
        Box::new(Listed {
            id: "second",
            sizes: &[11],
            fail: fail_second,
        }),
        Box::new(Listed {
            id: "third",
            sizes: &[13],
            fail: false,
        }),
    ]
}

fn opts() -> CleanOpts {
    CleanOpts::default()
}

fn shape(found: &CliDiscovery) -> (Vec<(String, u64)>, Vec<String>, bool) {
    (
        found
            .report
            .groups
            .iter()
            .map(|g| (g.id.clone(), g.total_size))
            .collect(),
        found.diagnostics.clone(),
        found.failed,
    )
}

#[test]
fn without_progress_or_cancel_it_is_the_cli_discovery() {
    for fail in [false, true] {
        let providers = providers(fail);
        let probe = MockChecker::none();
        let today = cli_discovery(
            discover_checked(&providers, &ScanContext::new(None, &probe), None),
            &opts(),
            &providers,
        );
        let with = cli_discovery_of(&providers, &opts(), &probe, None, None);
        assert_eq!(shape(&with), shape(&today));
        assert_eq!(with.failed, fail);
    }
}

#[test]
fn progress_names_each_category_as_it_starts_and_the_end() {
    let providers = providers(false);
    let events = Mutex::new(Vec::new());
    let record = |p: crate::progress::Progress| {
        events
            .lock()
            .unwrap()
            .push((p.completed, p.total, p.current_category));
    };
    let found = cli_discovery_of(
        &providers,
        &opts(),
        &MockChecker::none(),
        Some(&record),
        None,
    );
    assert!(!found.failed);
    assert_eq!(
        events.into_inner().unwrap(),
        [
            (0, Some(3), Some("first".to_string())),
            (1, Some(3), Some("second".to_string())),
            (2, Some(3), Some("third".to_string())),
            (3, Some(3), None),
        ]
    );
}

#[test]
fn a_cancelled_discovery_lists_what_was_read_and_counts_as_failed() {
    let providers = providers(false);
    let cancel = AtomicBool::new(false);
    // Stop once the second category starts: the first stays, the rest are not read.
    let stop = |p: crate::progress::Progress| {
        if p.current_category.as_deref() == Some("second") {
            cancel.store(true, Ordering::Release);
        }
    };
    let found = cli_discovery_of(
        &providers,
        &opts(),
        &MockChecker::none(),
        Some(&stop),
        Some(&cancel),
    );
    assert!(found.failed, "nothing may be cleaned from a stopped read");
    let ids: Vec<&str> = found.report.groups.iter().map(|g| g.id.as_str()).collect();
    assert!(ids.len() < 3, "{ids:?}");
    assert!(found
        .diagnostics
        .iter()
        .any(|d| d.contains("stopped before every category")));

    let before = AtomicBool::new(true);
    let found = cli_discovery_of(
        &providers,
        &opts(),
        &MockChecker::none(),
        None,
        Some(&before),
    );
    assert!(found.failed);
    assert!(found.report.groups.is_empty());
}

#[test]
fn every_category_that_fails_the_read_is_named_and_no_other() {
    let category = |id: &str, outcome| CheckedCategory {
        id: id.into(),
        label: id.into(),
        inclusion_reason: String::new(),
        risk: RiskLevel::Review,
        outcome,
    };
    let partial = CheckedItem {
        item: CleanItem {
            category_id: "partial".into(),
            category_label: "partial".into(),
            path: "/fixture/partial".into(),
            size: 1,
            risk: RiskLevel::Review,
            evidence: Vec::new(),
        },
        unreadable: Some(crate::clean::scan_context::Unreadable {
            entries: 1,
            first_error: "denied".into(),
        }),
        size_is_upper_bound: false,
        shared: crate::clean::freed_size::SharedBlocks::default(),
    };
    let found = |items, refused| CategoryOutcome::Found {
        items,
        unreadable: Vec::new(),
        roots: Vec::new(),
        refused,
    };
    let checked = CheckedDiscovery {
        categories: vec![
            category(
                "broken",
                CategoryOutcome::Failed {
                    error: "denied".into(),
                },
            ),
            category(
                "probe",
                CategoryOutcome::Unavailable {
                    reason: "pgrep failed".into(),
                },
            ),
            category(
                "running",
                CategoryOutcome::AppRunning {
                    app: "Xcode".into(),
                },
            ),
            category("partial", found(vec![partial], Vec::new())),
            category(
                "refusing",
                found(Vec::new(), vec![(PathBuf::from("/x"), "protected".into())]),
            ),
        ],
        cancelled: false,
    };
    let result = cli_discovery(checked, &opts(), &[]);
    assert!(result.failed);
    let ids: Vec<&str> = result
        .failed_categories
        .iter()
        .map(|(id, _)| id.as_str())
        .collect();
    assert_eq!(ids, ["broken", "probe", "partial"]);
    assert_eq!(result.failed_categories[0].1, "scan failed: denied");
    for (id, text) in &result.failed_categories {
        assert!(result.diagnostics.contains(&format!("{id}: {text}")));
    }
    assert_eq!(
        result.diagnostics.len(),
        4,
        "the refusal is reported, not failed"
    );

    let clean = cli_discovery(
        CheckedDiscovery {
            categories: vec![category("ok", found(Vec::new(), Vec::new()))],
            cancelled: false,
        },
        &opts(),
        &[],
    );
    assert!(!clean.failed && clean.failed_categories.is_empty());
}
