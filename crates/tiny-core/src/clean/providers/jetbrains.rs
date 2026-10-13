use std::path::PathBuf;

use crate::error::Result;

use super::{execute_per_item, top_level_entries, CleanProvider};
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};

const ID: &str = "jetbrains";
const LABEL: &str = "JetBrains caches/logs";

const JETBRAINS_DIRS: &[&str] = &["Library/Caches/JetBrains", "Library/Logs/JetBrains"];

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub struct JetBrains;

impl CleanProvider for JetBrains {
    fn id(&self) -> &'static str {
        ID
    }
    fn label(&self) -> &'static str {
        LABEL
    }
    fn inclusion_reason(&self) -> String {
        "JetBrains IDE caches and logs the IDE rebuilds".into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    fn desktop_trash_paths(&self) -> bool {
        true
    }
    fn available(&self) -> bool {
        let h = match home() {
            Some(h) => h,
            None => return false,
        };
        JETBRAINS_DIRS.iter().any(|s| h.join(s).is_dir())
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let h = match home() {
            Some(h) => h,
            None => return Ok(Vec::new()),
        };
        let mut items = Vec::new();
        for sub in JETBRAINS_DIRS {
            items.extend(top_level_entries(
                ctx,
                &h.join(sub),
                ID,
                LABEL,
                RiskLevel::Review,
            ));
        }
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
    fn jetbrains_id_in_known_categories() {
        assert!(known_category_ids().contains(&ID));
    }
}
