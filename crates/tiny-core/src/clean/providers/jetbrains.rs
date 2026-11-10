use std::path::{Path, PathBuf};

use crate::error::Result;

use super::{execute_per_item, top_level_entries, CleanProvider};
use crate::clean::fs_safe::{dir_size_checked, is_dir_safe, list_children};
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};

const ID: &str = "jetbrains";
const LABEL: &str = "JetBrains indexes/logs";

/// Holds each IDE's system directory, `<product><version>`.
const SYSTEMS: &str = "Library/Caches/JetBrains";
const LOGS: &str = "Library/Logs/JetBrains";
const JETBRAINS_DIRS: &[&str] = &[SYSTEMS, LOGS];
/// What an IDE rebuilds inside its system directory: the indexes
/// (IntelliJ's `PathManager.getIndexRoot`). Nothing else there is offered.
/// `LocalHistory` is the user's edit history, which nothing brings back, and
/// the IDE's own Invalidate Caches removes the file system cache only
/// "together with the information stored in Local History".
const REBUILT: &[&str] = &["index"];

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
        "JetBrains IDE indexes and logs the IDE rebuilds".into()
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
        Ok(items_in(&h, ctx))
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_per_item(items, action, ID)
    }
}

/// The rebuilt folders of every system directory under `home`, then the logs.
fn items_in(home: &Path, ctx: &ScanContext<'_>) -> Vec<CleanItem> {
    let mut items = Vec::new();
    for system in list_children(&home.join(SYSTEMS), ctx) {
        for path in REBUILT.iter().map(|name| system.join(name)) {
            if is_dir_safe(&path) {
                items.push(CleanItem {
                    category_id: ID.to_string(),
                    category_label: LABEL.to_string(),
                    size: dir_size_checked(&path, ctx),
                    path,
                    risk: RiskLevel::Review,
                    evidence: Vec::new(),
                });
            }
        }
    }
    items.extend(top_level_entries(
        ctx,
        &home.join(LOGS),
        ID,
        LABEL,
        RiskLevel::Review,
    ));
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::providers::known_category_ids;

    #[test]
    fn only_indexes_and_logs_are_offered_never_local_history() {
        let home = tempfile::tempdir().unwrap();
        let system = home.path().join(SYSTEMS).join("IntelliJIdea2025.2");
        for dir in ["index/stubs", "LocalHistory", "caches", "tmp"] {
            std::fs::create_dir_all(system.join(dir)).unwrap();
        }
        std::fs::write(system.join("LocalHistory/changes.storageData"), b"edits").unwrap();
        let logs = home.path().join(LOGS).join("IntelliJIdea2025.2");
        std::fs::create_dir_all(&logs).unwrap();
        let items = items_in(home.path(), &ScanContext::unchecked());
        let paths: Vec<_> = items.iter().map(|item| item.path.clone()).collect();
        assert_eq!(paths, [system.join("index"), logs]);
    }

    #[test]
    fn jetbrains_id_in_known_categories() {
        assert!(known_category_ids().contains(&ID));
    }
}
