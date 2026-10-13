use std::path::PathBuf;

use crate::error::Result;

use super::{execute_per_item, root_as_item, CleanProvider};
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};

const ID: &str = "gradle-maven";
const LABEL: &str = "Gradle/Maven caches";

const GRADLE_SUBDIRS: &[&str] = &[".gradle/caches", ".gradle/daemon"];
const MAVEN_SUBDIR: &str = ".m2/repository";

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub struct GradleMaven;

impl CleanProvider for GradleMaven {
    fn id(&self) -> &'static str {
        ID
    }
    fn label(&self) -> &'static str {
        LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Gradle and Maven caches re-downloaded on the next build".into()
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
        GRADLE_SUBDIRS.iter().any(|s| h.join(s).exists()) || h.join(MAVEN_SUBDIR).exists()
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let h = match home() {
            Some(h) => h,
            None => return Ok(Vec::new()),
        };
        let mut items = Vec::new();
        for sub in GRADLE_SUBDIRS {
            items.extend(root_as_item(
                ctx,
                &h.join(sub),
                ID,
                LABEL,
                RiskLevel::Review,
            ));
        }
        items.extend(root_as_item(
            ctx,
            &h.join(MAVEN_SUBDIR),
            ID,
            LABEL,
            RiskLevel::Review,
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
    fn gradle_maven_id_in_known_categories() {
        assert!(known_category_ids().contains(&ID));
    }
}
