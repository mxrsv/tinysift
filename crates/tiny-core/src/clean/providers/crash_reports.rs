use std::path::PathBuf;

use crate::error::Result;

use super::{execute_per_item, top_level_entries, CleanProvider};
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};

const ID: &str = "crash-reports";
const LABEL: &str = "Crash reports / DiagnosticReports";

const USER_DIAG: &str = "Library/Logs/DiagnosticReports";
const SYS_DIAG: &str = "/Library/Logs/DiagnosticReports";

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub struct CrashReports;

impl CleanProvider for CrashReports {
    fn id(&self) -> &'static str {
        ID
    }
    fn label(&self) -> &'static str {
        LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Crash and diagnostic reports kept only for troubleshooting".into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Safe
    }
    fn desktop_trash_paths(&self) -> bool {
        true
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let mut items = Vec::new();
        if let Some(h) = home() {
            items.extend(top_level_entries(
                ctx,
                &h.join(USER_DIAG),
                ID,
                LABEL,
                RiskLevel::Safe,
            ));
        }
        items.extend(top_level_entries(
            ctx,
            &PathBuf::from(SYS_DIAG),
            ID,
            LABEL,
            RiskLevel::Safe,
        ));
        Ok(items)
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_per_item(items, action, ID)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::providers::known_category_ids;

    #[test]
    fn crash_reports_id_in_known_categories() {
        assert!(known_category_ids().contains(&ID));
    }

    #[test]
    fn crash_reports_safe_risk() {
        assert_eq!(CrashReports.risk(), RiskLevel::Safe);
    }
}
