use crate::error::Result;

use super::{execute_per_item, top_level_entries, CleanProvider};
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};

const ID: &str = "user-logs";
const LABEL: &str = "User logs";

pub struct UserLogs;

impl CleanProvider for UserLogs {
    fn id(&self) -> &'static str {
        ID
    }

    fn label(&self) -> &'static str {
        LABEL
    }
    fn inclusion_reason(&self) -> String {
        "App logs kept only for troubleshooting".into()
    }

    fn risk(&self) -> RiskLevel {
        RiskLevel::Safe
    }
    fn desktop_trash_paths(&self) -> bool {
        true
    }

    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let home = match std::env::var_os("HOME") {
            Some(h) => std::path::PathBuf::from(h),
            None => return Ok(Vec::new()),
        };
        let root = home.join("Library/Logs");
        Ok(top_level_entries(ctx, &root, ID, LABEL, RiskLevel::Safe))
    }

    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_per_item(items, action, ID)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_trash_action() {
        let p = UserLogs;
        let err = p.execute(&[], ExecAction::EmptyTrash).unwrap_err();
        assert!(
            err.to_string().contains("does not accept EmptyTrash"),
            "expected EmptyTrash rejection, got: {err}"
        );
    }

    #[test]
    fn discover_returns_empty_when_logs_dir_missing() {
        // We don't manipulate $HOME here; instead exercise the helper
        // directly with a guaranteed-missing path.
        use crate::clean::providers::top_level_entries;
        use std::path::Path;
        let items = top_level_entries(
            &crate::clean::scan_context::ScanContext::unchecked(),
            Path::new("/tmp/__tiny_clean_logs_missing__"),
            ID,
            LABEL,
            RiskLevel::Safe,
        );
        assert!(items.is_empty());
    }
}
