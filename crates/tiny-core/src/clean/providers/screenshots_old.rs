use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::Result;

use super::{execute_per_item, is_idle, run_tool, CleanProvider};
use crate::clean::fs_safe::{dir_size_checked, is_dir_safe, list_children};
use crate::clean::runner::{CommandRunner, RealRunner};
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};

const ID: &str = "screenshots-old";
const LABEL: &str = "Old screenshots";

const SCREENSHOT_PREFIXES: &[&str] = &["Screenshot ", "Screen Shot "];

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub struct ScreenshotsOld {
    pub idle_days: u64,
    runner: Arc<dyn CommandRunner>,
}

impl ScreenshotsOld {
    pub fn new(idle_days: u64) -> Self {
        Self {
            idle_days,
            runner: Arc::new(RealRunner),
        }
    }
    pub fn with_runner(idle_days: u64, runner: Arc<dyn CommandRunner>) -> Self {
        Self { idle_days, runner }
    }
    fn screenshot_dir(&self) -> Result<Option<PathBuf>> {
        let out = run_tool(
            self.runner.as_ref(),
            "defaults",
            &["read", "com.apple.screencapture", "location"],
        )?;
        if out.success() {
            let trimmed = out.stdout.trim();
            if !trimmed.is_empty() {
                let p = PathBuf::from(expand_tilde(trimmed));
                if is_dir_safe(&p) {
                    return Ok(Some(p));
                }
            }
        }
        // Fallback: ~/Desktop (macOS default).
        Ok(home().map(|h| h.join("Desktop")).filter(|p| is_dir_safe(p)))
    }
}

impl CleanProvider for ScreenshotsOld {
    fn id(&self) -> &'static str {
        ID
    }
    fn label(&self) -> &'static str {
        LABEL
    }
    fn inclusion_reason(&self) -> String {
        format!(
            "Screenshots in the capture folder not modified for {} days",
            self.idle_days
        )
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    fn desktop_trash_paths(&self) -> bool {
        true
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let dir = match self.screenshot_dir()? {
            Some(d) => d,
            None => return Ok(Vec::new()),
        };
        Ok(list_old_screenshots(ctx, &dir, self.idle_days))
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_per_item(items, action, ID)
    }
}

/// Expands a leading `~` to `$HOME`. Bare `~` and `~/...` only — no
/// `~user/...` form.
fn expand_tilde(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("~/") {
        if let Some(h) = home() {
            return h.join(rest).to_string_lossy().into_owned();
        }
    }
    if s == "~" {
        if let Some(h) = home() {
            return h.to_string_lossy().into_owned();
        }
    }
    s.to_string()
}

/// Lists every file in `dir` whose name starts with a screenshot prefix
/// AND whose mtime is older than `idle_days`. Non-recursive.
pub fn list_old_screenshots(ctx: &ScanContext<'_>, dir: &Path, idle_days: u64) -> Vec<CleanItem> {
    let mut out = Vec::new();
    for path in list_children(dir, ctx) {
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.file_type().is_file() {
            continue;
        }
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if !SCREENSHOT_PREFIXES.iter().any(|p| name.starts_with(p)) {
            continue;
        }
        if !is_idle(&path, idle_days) {
            continue;
        }
        let size = dir_size_checked(&path, ctx);
        out.push(CleanItem {
            category_id: ID.to_string(),
            category_label: LABEL.to_string(),
            path,
            size,
            risk: RiskLevel::Review,
            evidence: Vec::new(),
        });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::providers::known_category_ids;
    use crate::clean::runner::test_support::MockRunner;
    use std::fs;

    fn tempdir(label: &str) -> PathBuf {
        let mut base = std::env::temp_dir();
        base.push(format!(
            "tiny-clean-ss-{}-{}",
            label,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&base).unwrap();
        base
    }

    fn backdate(p: &Path, days: u64) {
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86_400);
        std::fs::File::open(p).unwrap().set_modified(old).unwrap();
    }

    #[test]
    fn screenshots_old_id_in_known_categories() {
        assert!(known_category_ids().contains(&ID));
    }

    #[test]
    fn screenshot_dir_is_an_error_when_defaults_cannot_run() {
        let runner = Arc::new(MockRunner::new()); // no response → not found
        let p = ScreenshotsOld::with_runner(30, runner);
        assert!(p.screenshot_dir().is_err());
    }

    #[test]
    fn screenshot_dir_falls_back_to_desktop_when_key_missing() {
        // defaults read exits 1 (key unset) → fallback to ~/Desktop.
        let runner = Arc::new(MockRunner::new().with_exit(
            "defaults",
            &["read", "com.apple.screencapture", "location"],
            1,
            "",
        ));
        let p = ScreenshotsOld::with_runner(30, runner);
        let dir = p.screenshot_dir().unwrap();
        // We assume the test runner is on macOS where ~/Desktop exists. Fall
        // back path must end with "Desktop".
        if let Some(d) = dir {
            assert_eq!(d.file_name().and_then(|s| s.to_str()), Some("Desktop"));
        }
    }

    #[test]
    fn list_filters_by_prefix_and_age() {
        let dir = tempdir("filter");
        let ss_old = dir.join("Screenshot 2020-01-01 at 10.00.00.png");
        let ss_new = dir.join("Screenshot 2026-05-04 at 10.00.00.png");
        let other = dir.join("photo.png");
        fs::write(&ss_old, b"a").unwrap();
        fs::write(&ss_new, b"b").unwrap();
        fs::write(&other, b"c").unwrap();
        backdate(&ss_old, 60);
        backdate(&other, 60); // old but wrong prefix → must skip
        let found = list_old_screenshots(&ScanContext::unchecked(), &dir, 30);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, ss_old);
        let _ = crate::clean::fs_safe::remove_recursive_safe(&dir);
    }

    #[test]
    fn screenshot_dir_uses_defaults_value_when_present() {
        // Wire defaults to return a custom path that exists.
        let custom = tempdir("custom");
        let runner = Arc::new(MockRunner::new().with_exit(
            "defaults",
            &["read", "com.apple.screencapture", "location"],
            0,
            custom.to_str().unwrap(),
        ));
        let p = ScreenshotsOld::with_runner(30, runner);
        let dir = p.screenshot_dir().unwrap().unwrap();
        // Compare canonical paths: macOS resolves /var → /private/var
        // symlink, so the returned path may differ from the literal value
        // returned by `defaults`. Canonicalize both sides.
        let want = std::fs::canonicalize(&custom).unwrap();
        let got = std::fs::canonicalize(&dir).unwrap();
        assert_eq!(got, want);
        let _ = crate::clean::fs_safe::remove_recursive_safe(&custom);
    }
}
