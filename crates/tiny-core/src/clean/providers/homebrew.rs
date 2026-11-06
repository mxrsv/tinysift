//! Homebrew's own cleanup, `brew cleanup --prune=30`: old versions of
//! installed formulae, downloads older than 30 days, stale locks. Never a
//! formula itself: `TOOL_ENV` turns off the autoremove `brew cleanup` would
//! otherwise run.
//!
//! The category is one placeholder item, because one command removes paths
//! spread over the Homebrew prefix (`/opt/homebrew/Cellar`, its vendored
//! Ruby) that are no discovery root. Its size is the dry run's list sized by
//! TinySift's own walk, the rule the run is measured by
//! (`CleanProvider::tool_freeable`): before and after the cleanup the same
//! dry run is sized again, so the freed bytes are what left the disk. Paths
//! inside `~/Library/Caches` are left out of both: `user-caches` lists
//! Homebrew's download cache whole, so counting them here would count them
//! twice.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::engine_error;
use crate::error::Result;

use super::tool_command::run_tool_command;
use super::{execute_tool_command, run_tool, CleanProvider, DesktopTool};
use crate::clean::fs_safe::dir_size_checked;
use crate::clean::scan_context::ScanContext;
use crate::clean::trash_plan::inside_system;
use crate::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};
use crate::runner::CommandRunner;

const ID: &str = "homebrew";
const LABEL: &str = "Homebrew cleanup";
const BREW: &str = "brew";
/// Names no file: only `brew` acts on it.
pub(crate) const PLACEHOLDER: &str = "<homebrew:cleanup>";
const CLEANUP: &[&str] = &["cleanup", "--prune=30"];
const DRY_RUN: &[&str] = &["cleanup", "--prune=30", "--dry-run"];
const WOULD_REMOVE: &str = "Would remove: ";
/// `user-caches` lists everything here, Homebrew's download cache included.
const USER_CACHES: &str = "Library/Caches";

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub struct Homebrew {
    runner: Arc<dyn CommandRunner>,
}

impl Homebrew {
    pub fn with_runner(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }

    /// What `brew cleanup --prune=30` would free now, sized by TinySift's
    /// walk. A dry run that cannot run or fails is an error.
    fn freeable(&self, home: Option<&Path>) -> std::result::Result<u64, String> {
        let out = run_tool(self.runner.as_ref(), BREW, DRY_RUN).map_err(|e| e.to_string())?;
        if !out.success() {
            let reason = out.stderr.lines().map(str::trim).find(|l| !l.is_empty());
            return Err(format!(
                "brew cleanup --dry-run failed: {}",
                reason.unwrap_or("no error output")
            ));
        }
        Ok(would_remove(&out.stdout, home)
            .iter()
            .map(|path| size_of(path))
            .fold(0u64, u64::saturating_add))
    }
}

impl CleanProvider for Homebrew {
    fn owns_tool_id(&self, path: &Path) -> bool {
        path == Path::new(PLACEHOLDER)
    }
    fn id(&self) -> &'static str {
        ID
    }
    fn label(&self) -> &'static str {
        LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Old versions of Homebrew formulae and downloads older than 30 days; brew cleanup \
         removes them and Homebrew downloads them again if needed"
            .into()
    }
    /// Review: the old versions go for good whichever action is chosen, so
    /// `tiny clean` must not tick them under "Move to Trash (recoverable)".
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    fn available(&self) -> bool {
        self.runner.which(BREW)
    }
    fn required_tool(&self) -> Option<&'static str> {
        Some(BREW)
    }
    fn discover(&self, _ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let size = self
            .freeable(home().as_deref())
            .map_err(|e| engine_error!("{e}"))?;
        if size == 0 {
            return Ok(Vec::new());
        }
        Ok(vec![CleanItem {
            category_id: ID.to_string(),
            category_label: LABEL.to_string(),
            path: PathBuf::from(PLACEHOLDER),
            size,
            risk: RiskLevel::Review,
            evidence: Vec::new(),
        }])
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_tool_command(self.runner.as_ref(), items, action, ID, BREW, CLEANUP)
    }
    /// The dry run is the estimate, so the figure is sure.
    fn desktop_tool(&self) -> Option<DesktopTool> {
        self.runner
            .which(BREW)
            .then_some(DesktopTool::Remove { sure: true })
    }
    fn run_desktop_tool(&self, item: &CleanItem) -> std::result::Result<(), String> {
        self.check_item(&item.path)?;
        run_tool_command(self.runner.as_ref(), BREW, CLEANUP)
    }
    fn tool_freeable(&self, item: &CleanItem) -> Option<std::result::Result<u64, String>> {
        self.owns_tool_id(&item.path)
            .then(|| self.freeable(home().as_deref()))
    }
    fn check_item(&self, path: &Path) -> std::result::Result<(), String> {
        match self.owns_tool_id(path) {
            true => Ok(()),
            false => Err("not Homebrew's cleanup".into()),
        }
    }
}

/// The absolute paths `brew cleanup --dry-run` lists on stdout ("Would
/// remove: /opt/homebrew/Cellar/x/1.0 (12 files, 3MB)"), without `/System`
/// and without anything inside `home`'s `Library/Caches`. Warnings go to
/// stderr and are never read as paths.
fn would_remove(stdout: &str, home: Option<&Path>) -> Vec<PathBuf> {
    let caches = home.map(|h| h.join(USER_CACHES));
    stdout
        .lines()
        .filter_map(|line| line.strip_prefix(WOULD_REMOVE))
        .map(|rest| match rest.trim_end().strip_suffix(')') {
            Some(_) => rest.rfind(" (").map_or(rest, |at| &rest[..at]),
            None => rest.trim_end(),
        })
        .map(PathBuf::from)
        .filter(|path| path.is_absolute() && !inside_system(path))
        .filter(|path| !caches.as_ref().is_some_and(|c| path.starts_with(c)))
        .collect()
}

/// `path`'s freed bytes by the `freed_size` rule; 0 when it is gone.
fn size_of(path: &Path) -> u64 {
    match std::fs::symlink_metadata(path) {
        Err(e) if e.kind() == io::ErrorKind::NotFound => 0,
        _ => dir_size_checked(path, &ScanContext::unchecked()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::test_support::MockRunner;

    fn dry_run(lines: &[String]) -> String {
        let mut out = lines.join("\n");
        out.push_str("\n==> This operation would free approximately 1MB of disk space.\n");
        out
    }

    fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let keg = dir.path().join("Cellar/deno/2.6.6");
        std::fs::create_dir_all(keg.join("bin")).unwrap();
        std::fs::write(keg.join("bin/deno"), vec![1u8; 64 * 1024]).unwrap();
        let ruby = dir.path().join("vendor/portable-ruby/4.0.7_1");
        std::fs::create_dir_all(&ruby).unwrap();
        std::fs::write(ruby.join("ruby"), vec![2u8; 16 * 1024]).unwrap();
        let home = dir.path().join("home");
        let cached = home.join("Library/Caches/Homebrew/downloads/old--x.tar.gz");
        std::fs::create_dir_all(cached.parent().unwrap()).unwrap();
        std::fs::write(&cached, vec![3u8; 32 * 1024]).unwrap();
        (dir, keg, ruby, cached)
    }

    fn lines(keg: &Path, ruby: &Path, cached: &Path) -> Vec<String> {
        vec![
            format!("{WOULD_REMOVE}{} (12 files, 151.1MB)", keg.display()),
            format!("{WOULD_REMOVE}{} (1,707 files, 34.6MB)", ruby.display()),
            format!("{WOULD_REMOVE}{} (32KB)", cached.display()),
            format!("{WOULD_REMOVE}relative/path (1KB)"),
            format!("{WOULD_REMOVE}/System/Library/x (1KB)"),
            "Would prune 3 symbolic links from /opt/homebrew".into(),
        ]
    }

    #[test]
    fn the_dry_run_lists_paths_outside_system_and_the_user_caches() {
        let (dir, keg, ruby, cached) = fixture();
        let home = dir.path().join("home");
        let found = would_remove(&dry_run(&lines(&keg, &ruby, &cached)), Some(&home));
        assert_eq!(found, vec![keg, ruby], "{cached:?} is user-caches'");
    }

    #[test]
    fn homebrew_is_one_item_sized_from_its_dry_run_by_our_own_walk() {
        let (dir, keg, ruby, cached) = fixture();
        let home = dir.path().join("home");
        let stdout = dry_run(&lines(&keg, &ruby, &cached));
        let runner = Arc::new(
            MockRunner::new()
                .with_which(BREW)
                .with_exit(BREW, DRY_RUN, 0, &stdout),
        );
        let brew = Homebrew::with_runner(runner.clone());
        let size = brew.freeable(Some(&home)).unwrap();
        assert_eq!(size, size_of(&keg) + size_of(&ruby));
        assert!(size >= 80 * 1024, "{size}");
        assert_eq!(
            brew.desktop_tool(),
            Some(DesktopTool::Remove { sure: true })
        );
        assert!(!brew.desktop_trash_paths());
        assert_eq!(
            runner.output_calls.lock().unwrap().clone(),
            ["brew cleanup --prune=30 --dry-run"]
        );
        assert!(keg.exists(), "a dry run removes nothing");
    }

    #[test]
    fn nothing_to_free_lists_nothing_and_a_failed_dry_run_fails_the_category() {
        let quiet = Arc::new(
            MockRunner::new()
                .with_which(BREW)
                .with_exit(BREW, DRY_RUN, 0, ""),
        );
        let brew = Homebrew::with_runner(quiet);
        assert!(brew.discover(&ScanContext::unchecked()).unwrap().is_empty());
        let failing = Arc::new(
            MockRunner::new()
                .with_which(BREW)
                .with_exit(BREW, DRY_RUN, 1, ""),
        );
        assert!(Homebrew::with_runner(failing)
            .discover(&ScanContext::unchecked())
            .is_err());
        let missing = Homebrew::with_runner(Arc::new(MockRunner::new()));
        assert!(!missing.available());
        assert_eq!(missing.desktop_tool(), None);
    }

    #[test]
    fn the_desktop_runs_brew_cleanup_bounded_and_measures_the_dry_run_again() {
        let item = CleanItem {
            category_id: ID.into(),
            category_label: LABEL.into(),
            path: PathBuf::from(PLACEHOLDER),
            size: 1,
            risk: RiskLevel::Review,
            evidence: Vec::new(),
        };
        let runner = Arc::new(
            MockRunner::new()
                .with_which(BREW)
                .with_exit(BREW, CLEANUP, 0, "")
                .with_exit(BREW, DRY_RUN, 0, ""),
        );
        let brew = Homebrew::with_runner(runner.clone());
        assert_eq!(brew.run_desktop_tool(&item), Ok(()));
        assert_eq!(brew.tool_freeable(&item), Some(Ok(0)));
        let other = CleanItem {
            path: PathBuf::from("/opt/homebrew/Cellar"),
            ..item.clone()
        };
        assert!(brew.run_desktop_tool(&other).is_err());
        assert_eq!(brew.tool_freeable(&other), None);
        assert_eq!(
            runner.output_calls.lock().unwrap().clone(),
            [
                "brew cleanup --prune=30",
                "brew cleanup --prune=30 --dry-run"
            ]
        );
        assert!(runner.run_calls.lock().unwrap().is_empty());
    }

    #[test]
    fn the_cli_runs_the_same_cleanup_once() {
        let runner = Arc::new(
            MockRunner::new()
                .with_which(BREW)
                .with_response(BREW, CLEANUP, true, ""),
        );
        let item = CleanItem {
            category_id: ID.into(),
            category_label: LABEL.into(),
            path: PathBuf::from(PLACEHOLDER),
            size: 1,
            risk: RiskLevel::Review,
            evidence: Vec::new(),
        };
        let report = Homebrew::with_runner(runner.clone())
            .execute(&[item], ExecAction::HardDelete)
            .unwrap();
        assert_eq!(report.removed_paths, vec![PathBuf::from(PLACEHOLDER)]);
        assert_eq!(
            runner.run_calls.lock().unwrap().clone(),
            ["brew cleanup --prune=30"]
        );
    }

    #[test]
    fn brew_never_autoremoves_a_formula() {
        let env = crate::runner::TOOL_ENV
            .iter()
            .find(|(tool, _)| *tool == BREW)
            .map(|(_, env)| *env);
        assert_eq!(env, Some(&[("HOMEBREW_NO_AUTOREMOVE", "1")][..]));
    }
}
