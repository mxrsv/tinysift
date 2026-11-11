//! Homebrew's own cleanup, `brew cleanup --prune=30`: old versions of
//! installed formulae, downloads older than 30 days, stale locks. Never a
//! formula itself: `TOOL_ENV` turns off the autoremove `brew cleanup` would
//! otherwise run.
//!
//! The category is one placeholder item, because one command removes paths
//! spread over the Homebrew prefix (`/opt/homebrew/Cellar`, its vendored
//! Ruby) that are no discovery root. Its size is the dry run's list sized by
//! TinySift's own walk, the rule the run is measured by
//! (`CleanProvider::tool_freeable`): right before and right after the
//! cleanup the same list is sized again, so the freed bytes are what left
//! the disk. The list is the scan's while it is fresh (`LISTING_FRESH`),
//! shared by the scan's and the run's providers, so a Clean spawns `brew`
//! once, for the cleanup itself. Paths
//! inside `~/Library/Caches` are left out of both: `user-caches` lists
//! Homebrew's download cache whole, so counting them here would count them
//! twice.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, Instant};

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

/// How long a dry run's list stands for a Clean: a preview's lifetime, so a
/// Clean confirmed from a scan reuses the scan's list.
const LISTING_FRESH: Duration = Duration::from_secs(15 * 60);

/// The paths the latest dry run listed, and when.
#[derive(Debug, Clone)]
struct Listing {
    at: Instant,
    paths: Vec<PathBuf>,
}

type SharedListing = Arc<Mutex<Option<Listing>>>;

/// The app's providers are built anew for a run, so the scan's list lives
/// here, not in one provider.
static LAST_LISTING: LazyLock<SharedListing> = LazyLock::new(SharedListing::default);

pub struct Homebrew {
    runner: Arc<dyn CommandRunner>,
    listing: SharedListing,
}

impl Homebrew {
    pub fn with_runner(runner: Arc<dyn CommandRunner>) -> Self {
        Self {
            runner,
            listing: LAST_LISTING.clone(),
        }
    }

    /// The same provider with a list of its own, so tests never share one.
    #[cfg(test)]
    fn isolated(runner: Arc<dyn CommandRunner>) -> Self {
        Self {
            runner,
            listing: SharedListing::default(),
        }
    }

    /// Runs the dry run now and keeps its list. A dry run that cannot run
    /// or fails is an error.
    fn list(&self, home: Option<&Path>) -> std::result::Result<Vec<PathBuf>, String> {
        let out = run_tool(self.runner.as_ref(), BREW, DRY_RUN).map_err(|e| e.to_string())?;
        if !out.success() {
            let reason = out.stderr.lines().map(str::trim).find(|l| !l.is_empty());
            return Err(format!(
                "brew cleanup --dry-run failed: {}",
                reason.unwrap_or("no error output")
            ));
        }
        let paths = would_remove(&out.stdout, home);
        let listing = Listing {
            at: Instant::now(),
            paths: paths.clone(),
        };
        *self.listing.lock().unwrap_or_else(|e| e.into_inner()) = Some(listing);
        Ok(paths)
    }

    /// The kept list while it is fresh.
    fn fresh_list(&self) -> Option<Vec<PathBuf>> {
        let kept = self.listing.lock().unwrap_or_else(|e| e.into_inner());
        kept.as_ref()
            .filter(|listing| listing.at.elapsed() < LISTING_FRESH)
            .map(|listing| listing.paths.clone())
    }

    /// What the listed paths hold now, by TinySift's walk: the kept list
    /// while fresh, a new dry run's otherwise. Before and after a cleanup
    /// both size the same list, so the second never spawns `brew`.
    fn freeable(&self, home: Option<&Path>) -> std::result::Result<u64, String> {
        let paths = match self.fresh_list() {
            Some(paths) => paths,
            None => self.list(home)?,
        };
        Ok(sized(&paths))
    }
}

fn sized(paths: &[PathBuf]) -> u64 {
    paths
        .iter()
        .map(|path| size_of(path))
        .fold(0u64, u64::saturating_add)
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
        // A scan always asks brew afresh.
        let paths = self
            .list(home().as_deref())
            .map_err(|e| engine_error!("{e}"))?;
        let size = sized(&paths);
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
    /// `brew cleanup` deletes old downloads and logs in these folders;
    /// Homebrew's own variables move them.
    fn tool_touches(&self) -> Vec<PathBuf> {
        let Some(home) = home() else {
            return Vec::new();
        };
        [
            ("HOMEBREW_CACHE", "Library/Caches/Homebrew"),
            ("HOMEBREW_LOGS", "Library/Logs/Homebrew"),
        ]
        .into_iter()
        .map(|(variable, default)| {
            std::env::var_os(variable)
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .unwrap_or_else(|| home.join(default))
        })
        .collect()
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

    #[test]
    fn cleanup_names_the_download_cache_and_logs_it_deletes_in() {
        let touches = Homebrew::with_runner(Arc::new(MockRunner::new())).tool_touches();
        assert_eq!(touches.len(), 2);
        assert!(touches
            .iter()
            .all(|path| path.is_absolute() && path.ends_with("Homebrew")));
    }

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
        let brew = Homebrew::isolated(runner.clone());
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
        let brew = Homebrew::isolated(quiet);
        assert!(brew.discover(&ScanContext::unchecked()).unwrap().is_empty());
        let failing = Arc::new(
            MockRunner::new()
                .with_which(BREW)
                .with_exit(BREW, DRY_RUN, 1, ""),
        );
        assert!(Homebrew::isolated(failing)
            .discover(&ScanContext::unchecked())
            .is_err());
        let missing = Homebrew::isolated(Arc::new(MockRunner::new()));
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
        let brew = Homebrew::isolated(runner.clone());
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

    /// A Clean right after a scan spawns brew once, for the cleanup: before
    /// and after are the scan's list sized again, so freed is what left it.
    #[test]
    fn a_clean_after_a_scan_measures_the_scans_list_without_another_dry_run() {
        let (dir, keg, ruby, cached) = fixture();
        let stdout = dry_run(&lines(&keg, &ruby, &cached));
        let runner = Arc::new(
            MockRunner::new()
                .with_which(BREW)
                .with_exit(BREW, DRY_RUN, 0, &stdout)
                .with_exit(BREW, CLEANUP, 0, ""),
        );
        let brew = Homebrew::isolated(runner.clone());
        let home = dir.path().join("home");
        let scanned = brew.list(Some(&home)).unwrap();
        assert_eq!(scanned, vec![keg.clone(), ruby.clone()]);
        let item = CleanItem {
            category_id: ID.into(),
            category_label: LABEL.into(),
            path: PathBuf::from(PLACEHOLDER),
            size: 1,
            risk: RiskLevel::Review,
            evidence: Vec::new(),
        };
        let keg_size = size_of(&keg);
        let Some(Ok(before)) = brew.tool_freeable(&item) else {
            panic!("measured")
        };
        assert_eq!(brew.run_desktop_tool(&item), Ok(()));
        std::fs::remove_dir_all(&keg).unwrap(); // what the cleanup removed
        let Some(Ok(after)) = brew.tool_freeable(&item) else {
            panic!("measured")
        };
        assert_eq!(
            before - after,
            keg_size,
            "freed is what left the listed paths"
        );
        assert_eq!(
            runner.output_calls.lock().unwrap().clone(),
            [
                "brew cleanup --prune=30 --dry-run",
                "brew cleanup --prune=30"
            ],
            "the scan's dry run, then only the cleanup"
        );
        // A list older than a preview's lifetime is asked again.
        let old = Instant::now().checked_sub(LISTING_FRESH + Duration::from_secs(1));
        if let Some(at) = old {
            brew.listing.lock().unwrap().as_mut().unwrap().at = at;
            brew.tool_freeable(&item);
            assert_eq!(runner.output_calls.lock().unwrap().len(), 3);
        }
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
        let report = Homebrew::isolated(runner.clone())
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
