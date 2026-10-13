//! Local Time Machine snapshots — synthetic provider, no filesystem path
//! to remove. Discovery shells out to `tmutil listlocalsnapshots /`,
//! execute shells out to `tmutil deletelocalsnapshots <date>`.
//!
//! Risk = **Destructive**. Snapshot deletion is not revertible (no Trash
//! semantics), so Trash and HardDelete both map to the same `tmutil`
//! call; we warn the user when they pick Trash.

use std::path::PathBuf;
use std::sync::Arc;

use crate::engine_error;
use crate::error::Result;

use super::{run_tool, CleanProvider};
use crate::clean::runner::{CommandRunner, RealRunner};
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};

const ID: &str = "time-machine-local";
const LABEL: &str = "Time Machine local snapshots";

const PLACEHOLDER_PREFIX: &str = "<tmutil:";
const PLACEHOLDER_SUFFIX: &str = ">";
const SNAPSHOT_PREFIX: &str = "com.apple.TimeMachine.";
const SNAPSHOT_SUFFIX: &str = ".local";

pub struct TimeMachineLocal {
    runner: Arc<dyn CommandRunner>,
}

impl TimeMachineLocal {
    pub fn new() -> Self {
        Self {
            runner: Arc::new(RealRunner),
        }
    }
    pub fn with_runner(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }
}

impl Default for TimeMachineLocal {
    fn default() -> Self {
        Self::new()
    }
}

impl CleanProvider for TimeMachineLocal {
    fn id(&self) -> &'static str {
        ID
    }
    fn label(&self) -> &'static str {
        LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Local Time Machine snapshots macOS also thins on its own".into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Destructive
    }
    fn available(&self) -> bool {
        self.runner.which("tmutil")
    }
    fn required_tool(&self) -> Option<&'static str> {
        Some("tmutil")
    }
    fn discover(&self, _ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let out = run_tool(self.runner.as_ref(), "tmutil", &["listlocalsnapshots", "/"])?;
        if !out.success() {
            return Ok(Vec::new());
        }
        let mut items = Vec::new();
        for snapshot in parse_snapshots(&out.stdout) {
            let placeholder = format!("{}{}{}", PLACEHOLDER_PREFIX, snapshot, PLACEHOLDER_SUFFIX);
            items.push(CleanItem {
                category_id: ID.to_string(),
                category_label: LABEL.to_string(),
                path: PathBuf::from(placeholder),
                // tmutil doesn't expose snapshot size; report 0 so we
                // don't lie to the user. Picker still flags by count.
                size: 0,
                risk: RiskLevel::Destructive,
                evidence: Vec::new(),
            });
        }
        Ok(items)
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        if matches!(action, ExecAction::EmptyTrash) {
            return Err(engine_error!(
                "time_machine_local does not accept EmptyTrash"
            ));
        }

        let mut report = ExecReport::default();
        for item in items {
            let snapshot = match parse_placeholder(&item.path) {
                Some(s) => s,
                None => {
                    report.failed.push((
                        item.path.clone(),
                        "internal: malformed tmutil placeholder".into(),
                    ));
                    continue;
                }
            };
            let date = match snapshot_to_date(&snapshot) {
                Some(d) => d,
                None => {
                    report.failed.push((
                        item.path.clone(),
                        format!("snapshot id has unexpected format: {}", snapshot),
                    ));
                    continue;
                }
            };
            let out = self.runner.run("tmutil", &["deletelocalsnapshots", &date]);
            if out.success {
                report.removed_paths.push(item.path.clone());
            } else {
                report.failed.push((
                    item.path.clone(),
                    format!("tmutil deletelocalsnapshots {} failed", date),
                ));
            }
        }
        Ok(report)
    }
}

/// Parses `tmutil listlocalsnapshots /` output. Lines that don't look like
/// a snapshot id (header, blank) are skipped.
pub fn parse_snapshots(stdout: &str) -> Vec<String> {
    stdout
        .lines()
        .map(|l| l.trim())
        .filter(|l| l.starts_with(SNAPSHOT_PREFIX))
        .map(|l| l.to_string())
        .collect()
}

/// `<tmutil:com.apple.TimeMachine.2024-12-15-103045.local>` →
/// `com.apple.TimeMachine.2024-12-15-103045.local`. Returns None when
/// the wrapping doesn't match (defensive against execute being called
/// with a non-tmutil path).
pub fn parse_placeholder(path: &std::path::Path) -> Option<String> {
    let s = path.to_str()?;
    let inner = s.strip_prefix(PLACEHOLDER_PREFIX)?;
    let inner = inner.strip_suffix(PLACEHOLDER_SUFFIX)?;
    Some(inner.to_string())
}

/// `com.apple.TimeMachine.2024-12-15-103045.local` → `2024-12-15-103045`
/// (the date format `tmutil deletelocalsnapshots` expects).
pub fn snapshot_to_date(snapshot: &str) -> Option<String> {
    let inner = snapshot.strip_prefix(SNAPSHOT_PREFIX)?;
    let inner = inner.strip_suffix(SNAPSHOT_SUFFIX)?;
    if inner.is_empty() {
        return None;
    }
    Some(inner.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::providers::known_category_ids;
    use crate::clean::runner::test_support::MockRunner;

    #[test]
    fn time_machine_id_in_known_categories() {
        assert!(known_category_ids().contains(&ID));
    }

    #[test]
    fn time_machine_destructive_risk() {
        assert_eq!(TimeMachineLocal::new().risk(), RiskLevel::Destructive);
    }

    #[test]
    fn parse_snapshots_skips_header() {
        let out = "Snapshots for volume group containing disk /:\n\
                   com.apple.TimeMachine.2024-12-15-103045.local\n\
                   com.apple.TimeMachine.2024-12-16-103045.local\n\
                   \n";
        let snaps = parse_snapshots(out);
        assert_eq!(snaps.len(), 2);
        assert_eq!(snaps[0], "com.apple.TimeMachine.2024-12-15-103045.local");
    }

    #[test]
    fn snapshot_to_date_strips_prefix_and_suffix() {
        let date = snapshot_to_date("com.apple.TimeMachine.2024-12-15-103045.local").unwrap();
        assert_eq!(date, "2024-12-15-103045");
    }

    #[test]
    fn snapshot_to_date_rejects_malformed() {
        assert!(snapshot_to_date("bogus").is_none());
        assert!(snapshot_to_date("com.apple.TimeMachine..local").is_none()); // empty inner
        assert!(snapshot_to_date("com.apple.TimeMachine.2024").is_none()); // no .local suffix
    }

    #[test]
    fn parse_placeholder_round_trips() {
        let p = PathBuf::from("<tmutil:com.apple.TimeMachine.2024-12-15-103045.local>");
        let snap = parse_placeholder(&p).unwrap();
        assert_eq!(snap, "com.apple.TimeMachine.2024-12-15-103045.local");
    }

    #[test]
    fn parse_placeholder_rejects_wrong_wrapping() {
        assert!(parse_placeholder(std::path::Path::new("/tmp/foo")).is_none());
        assert!(parse_placeholder(std::path::Path::new("<docker:images>")).is_none());
    }

    #[test]
    fn discover_returns_empty_when_tmutil_fails() {
        let runner =
            Arc::new(MockRunner::new().with_exit("tmutil", &["listlocalsnapshots", "/"], 1, ""));
        let p = TimeMachineLocal::with_runner(runner);
        assert!(p.discover(&ScanContext::unchecked()).unwrap().is_empty());
    }

    #[test]
    fn discover_parses_snapshot_list() {
        let out = "Snapshots for volume group containing disk /:\n\
                   com.apple.TimeMachine.2025-01-01-000000.local\n";
        let runner =
            Arc::new(MockRunner::new().with_exit("tmutil", &["listlocalsnapshots", "/"], 0, out));
        let p = TimeMachineLocal::with_runner(runner);
        let items = p.discover(&ScanContext::unchecked()).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].size, 0);
        assert!(items[0]
            .path
            .to_string_lossy()
            .contains("2025-01-01-000000"));
    }

    #[test]
    fn execute_calls_tmutil_with_date() {
        let runner = Arc::new(MockRunner::new().with_response(
            "tmutil",
            &["deletelocalsnapshots", "2025-01-01-000000"],
            true,
            "",
        ));
        let p = TimeMachineLocal::with_runner(runner);
        let item = CleanItem {
            category_id: ID.into(),
            category_label: LABEL.into(),
            path: PathBuf::from("<tmutil:com.apple.TimeMachine.2025-01-01-000000.local>"),
            size: 0,
            risk: RiskLevel::Destructive,
            evidence: Vec::new(),
        };
        let report = p.execute(&[item], ExecAction::HardDelete).unwrap();
        assert_eq!(report.removed_paths.len(), 1);
        assert!(report.failed.is_empty());
    }

    #[test]
    fn execute_reports_failure_when_tmutil_fails() {
        let runner = Arc::new(MockRunner::new().with_response(
            "tmutil",
            &["deletelocalsnapshots", "2025-01-01-000000"],
            false,
            "",
        ));
        let p = TimeMachineLocal::with_runner(runner);
        let item = CleanItem {
            category_id: ID.into(),
            category_label: LABEL.into(),
            path: PathBuf::from("<tmutil:com.apple.TimeMachine.2025-01-01-000000.local>"),
            size: 0,
            risk: RiskLevel::Destructive,
            evidence: Vec::new(),
        };
        let report = p.execute(&[item], ExecAction::HardDelete).unwrap();
        assert!(report.removed_paths.is_empty());
        assert_eq!(report.failed.len(), 1);
    }
}
