use std::path::PathBuf;

use crate::error::Result;

use super::{execute_per_item, root_as_item, CleanProvider};
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};

const ID: &str = "quarantine";
const LABEL: &str = "Gatekeeper quarantine events";

/// Single-file path: stores Gatekeeper "open from unknown developer"
/// prompt history. Risk is Review because deleting clears the recorded
/// approvals (user may re-prompt next time).
const QUARANTINE_FILE: &str = "Library/Preferences/com.apple.LaunchServices.QuarantineEventsV2";

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub struct Quarantine;

impl CleanProvider for Quarantine {
    fn id(&self) -> &'static str {
        ID
    }
    fn label(&self) -> &'static str {
        LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Gatekeeper history of opened downloads; clearing it may bring back first-open prompts"
            .into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    fn desktop_trash_paths(&self) -> bool {
        true
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let h = match home() {
            Some(h) => h,
            None => return Ok(Vec::new()),
        };
        Ok(root_as_item(
            ctx,
            &h.join(QUARANTINE_FILE),
            ID,
            LABEL,
            RiskLevel::Review,
        ))
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
    fn quarantine_id_in_known_categories() {
        assert!(known_category_ids().contains(&ID));
    }

    #[test]
    fn quarantine_review_risk() {
        assert_eq!(Quarantine.risk(), RiskLevel::Review);
    }
}
