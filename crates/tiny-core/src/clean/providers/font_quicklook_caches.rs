use std::path::PathBuf;
use std::sync::Arc;

use crate::error::Result;

use super::{execute_per_item, root_as_item, run_tool, CleanProvider};
use crate::clean::runner::{CommandRunner, RealRunner};
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};

const ID: &str = "font-quicklook-caches";
const LABEL: &str = "Font + QuickLook caches";

/// `~/Library/Caches/...` paths. The `getconf DARWIN_USER_CACHE_DIR` path
/// is resolved at discover time.
const USER_QL: &str = "Library/Caches/com.apple.QuickLook.thumbnailcache";
const FONT_REGISTRY: &str = "Library/Caches/com.apple.FontRegistry";

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub struct FontQuicklookCaches {
    runner: Arc<dyn CommandRunner>,
}

impl FontQuicklookCaches {
    pub fn new() -> Self {
        Self {
            runner: Arc::new(RealRunner),
        }
    }
    pub fn with_runner(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }
    fn darwin_user_cache_dir(&self) -> Result<Option<PathBuf>> {
        let out = run_tool(self.runner.as_ref(), "getconf", &["DARWIN_USER_CACHE_DIR"])?;
        if !out.success() {
            return Ok(None);
        }
        let trimmed = out.stdout.trim();
        if trimmed.is_empty() {
            return Ok(None);
        }
        Ok(Some(PathBuf::from(trimmed)))
    }
}

impl Default for FontQuicklookCaches {
    fn default() -> Self {
        Self::new()
    }
}

impl CleanProvider for FontQuicklookCaches {
    fn id(&self) -> &'static str {
        ID
    }
    fn label(&self) -> &'static str {
        LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Font and Quick Look thumbnail caches macOS rebuilds".into()
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
            items.extend(root_as_item(
                ctx,
                &h.join(USER_QL),
                ID,
                LABEL,
                RiskLevel::Safe,
            ));
            items.extend(root_as_item(
                ctx,
                &h.join(FONT_REGISTRY),
                ID,
                LABEL,
                RiskLevel::Safe,
            ));
        }
        if let Some(darwin_cache) = self.darwin_user_cache_dir()? {
            // /private/var/folders/.../C/com.apple.QuickLook.thumbnailcache
            let p = darwin_cache.join("com.apple.QuickLook.thumbnailcache");
            items.extend(root_as_item(ctx, &p, ID, LABEL, RiskLevel::Safe));
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
    use crate::clean::runner::test_support::MockRunner;

    #[test]
    fn font_quicklook_id_in_known_categories() {
        assert!(known_category_ids().contains(&ID));
    }

    #[test]
    fn font_quicklook_safe_risk() {
        assert_eq!(FontQuicklookCaches::new().risk(), RiskLevel::Safe);
    }

    #[test]
    fn darwin_cache_dir_is_an_error_when_getconf_cannot_run() {
        let runner = Arc::new(MockRunner::new()); // no response → not found
        let p = FontQuicklookCaches::with_runner(runner);
        assert!(p.darwin_user_cache_dir().is_err());
    }

    #[test]
    fn darwin_cache_dir_is_none_when_getconf_fails() {
        let runner =
            Arc::new(MockRunner::new().with_exit("getconf", &["DARWIN_USER_CACHE_DIR"], 1, ""));
        let p = FontQuicklookCaches::with_runner(runner);
        assert!(p.darwin_user_cache_dir().unwrap().is_none());
    }

    #[test]
    fn darwin_cache_dir_parses_getconf_output() {
        let runner = Arc::new(MockRunner::new().with_exit(
            "getconf",
            &["DARWIN_USER_CACHE_DIR"],
            0,
            "/private/var/folders/ab/cdef1234/C/\n",
        ));
        let p = FontQuicklookCaches::with_runner(runner);
        let dir = p.darwin_user_cache_dir().unwrap().unwrap();
        // getconf typically prints with a trailing slash; PathBuf preserves it.
        assert_eq!(dir, PathBuf::from("/private/var/folders/ab/cdef1234/C/"));
    }
}
