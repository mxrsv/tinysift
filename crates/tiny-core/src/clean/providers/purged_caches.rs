//! Package caches whose own tool empties them: Bun (`bun pm cache rm`) and
//! pip (`pip3 cache purge`). Neither has a dry run, so like a uv or pnpm
//! prune the cache's size is only a ceiling ("up to"), never part of a sure
//! figure, and in Trash mode the folder moves to the Trash instead.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::Result;

use super::dev_caches::{prunable_cache, still_tool_path, tool_path};
use super::tool_command::run_tool_command;
use super::{execute_tool_command, CleanProvider, DesktopTool};
use crate::clean::fs_safe::is_dir_safe;
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};
use crate::runner::CommandRunner;

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

// ---------- Bun ----------

const BUN_ID: &str = "bun-cache";
const BUN_LABEL: &str = "Bun cache";
const BUN: &str = "bun";
const BUN_CACHE_DIR: &[&str] = &["pm", "cache"];
const BUN_CLEAR: &[&str] = &["pm", "cache", "rm"];

pub struct BunCache {
    runner: Arc<dyn CommandRunner>,
}

impl BunCache {
    pub fn with_runner(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }
}

impl CleanProvider for BunCache {
    fn id(&self) -> &'static str {
        BUN_ID
    }
    fn label(&self) -> &'static str {
        BUN_LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Bun's package cache; bun pm cache rm empties it and Bun downloads packages again when \
         needed"
            .into()
    }
    /// Review: emptied for good whichever action is chosen.
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    /// An install in progress reads the cache.
    fn requires_app_quit(&self) -> Option<&'static str> {
        Some(BUN)
    }
    fn available(&self) -> bool {
        self.runner.which(BUN)
    }
    fn required_tool(&self) -> Option<&'static str> {
        Some(BUN)
    }
    fn roots_from_tool_output(&self) -> bool {
        true
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        match tool_path(self.runner.as_ref(), BUN, BUN_CACHE_DIR)? {
            Some(path) if path.is_absolute() => Ok(prunable_cache(
                ctx,
                &path,
                BUN_ID,
                BUN_LABEL,
                RiskLevel::Review,
            )),
            _ => Ok(Vec::new()),
        }
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_tool_command(self.runner.as_ref(), items, action, BUN_ID, BUN, BUN_CLEAR)
    }
    fn desktop_tool(&self) -> Option<DesktopTool> {
        self.runner.which(BUN).then_some(DesktopTool::Prune)
    }
    fn run_desktop_tool(&self, _item: &CleanItem) -> std::result::Result<(), String> {
        run_tool_command(self.runner.as_ref(), BUN, BUN_CLEAR)
    }
    fn check_item(&self, path: &Path) -> std::result::Result<(), String> {
        still_tool_path(self.runner.as_ref(), BUN, BUN_CACHE_DIR, path)
    }
}

// ---------- pip ----------

const PIP_ID: &str = "pip-cache";
const PIP_LABEL: &str = "pip cache";
const PIP_CACHE_DIR: &[&str] = &["cache", "dir"];
const PIP_PURGE: &[&str] = &["cache", "purge"];
/// pip's cache on macOS unless `PIP_CACHE_DIR` moves it.
const PIP_DEFAULT_DIR: &str = "Library/Caches/pip";
/// Real `pip3` binaries, never `/usr/bin/pip3`: without the Command Line
/// Tools that shim asks to install them, a dialog a scan must never raise.
const PIPS: &[&str] = &[
    "/opt/homebrew/bin/pip3",
    "/usr/local/bin/pip3",
    "/Library/Developer/CommandLineTools/usr/bin/pip3",
];

pub struct PipCache {
    runner: Arc<dyn CommandRunner>,
}

impl PipCache {
    pub fn with_runner(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }

    fn pip(&self) -> Option<&'static str> {
        PIPS.iter().copied().find(|pip| self.runner.which(pip))
    }
}

impl CleanProvider for PipCache {
    fn id(&self) -> &'static str {
        PIP_ID
    }
    fn label(&self) -> &'static str {
        PIP_LABEL
    }
    fn inclusion_reason(&self) -> String {
        "pip's cache of downloaded wheels and pages; pip3 cache purge empties it and pip \
         downloads them again when needed"
            .into()
    }
    /// Review: emptied for good whichever action is chosen.
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    /// Only when the default cache exists, so nothing is spawned otherwise.
    /// Without a `pip3` it is still reported; it cannot be purged then.
    fn available(&self) -> bool {
        home().is_some_and(|h| is_dir_safe(&h.join(PIP_DEFAULT_DIR)))
    }
    fn roots_from_tool_output(&self) -> bool {
        true
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let Some(h) = home() else {
            return Ok(Vec::new());
        };
        pip_cache(ctx, self.runner.as_ref(), self.pip(), &h)
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        let pip = self.pip().unwrap_or(PIPS[0]);
        execute_tool_command(self.runner.as_ref(), items, action, PIP_ID, pip, PIP_PURGE)
    }
    /// Never without a real `pip3`: the default cache alone stays report only.
    fn desktop_tool(&self) -> Option<DesktopTool> {
        self.pip().map(|_| DesktopTool::Prune)
    }
    fn run_desktop_tool(&self, _item: &CleanItem) -> std::result::Result<(), String> {
        let pip = self.pip().ok_or("pip3 was not found")?;
        run_tool_command(self.runner.as_ref(), pip, PIP_PURGE)
    }
    fn check_item(&self, path: &Path) -> std::result::Result<(), String> {
        let pip = self.pip().ok_or("pip3 was not found")?;
        still_tool_path(self.runner.as_ref(), pip, PIP_CACHE_DIR, path)
    }
}

/// The cache `pip` names, which must be absolute, or the default cache
/// under `home` when no `pip3` is found. Nothing when the default is absent.
fn pip_cache(
    ctx: &ScanContext<'_>,
    runner: &dyn CommandRunner,
    pip: Option<&str>,
    home: &Path,
) -> Result<Vec<CleanItem>> {
    let default = home.join(PIP_DEFAULT_DIR);
    if !is_dir_safe(&default) {
        return Ok(Vec::new());
    }
    let path = match pip {
        Some(pip) => match tool_path(runner, pip, PIP_CACHE_DIR)? {
            Some(path) if path.is_absolute() => path,
            _ => return Ok(Vec::new()),
        },
        None => default,
    };
    Ok(prunable_cache(
        ctx,
        &path,
        PIP_ID,
        PIP_LABEL,
        RiskLevel::Review,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::test_support::MockRunner;

    const BREW_PIP: &str = "/opt/homebrew/bin/pip3";

    fn cache_with_a_file(path: &Path) {
        std::fs::create_dir_all(path.join("wheels")).unwrap();
        std::fs::write(path.join("wheels/w"), vec![1u8; 8192]).unwrap();
    }

    fn printed(path: &Path) -> String {
        format!("{}\n", path.display())
    }

    fn item(id: &str, path: &Path) -> CleanItem {
        CleanItem {
            category_id: id.into(),
            category_label: id.into(),
            path: path.to_path_buf(),
            size: 1,
            risk: RiskLevel::Review,
            evidence: Vec::new(),
        }
    }

    #[test]
    fn bun_lists_the_cache_it_names_as_a_ceiling_and_empties_it_with_its_own_command() {
        let dir = tempfile::tempdir().unwrap();
        let cache = dir.path().join("install/cache");
        cache_with_a_file(&cache);
        let runner = Arc::new(
            MockRunner::new()
                .with_which(BUN)
                .with_exit(BUN, BUN_CACHE_DIR, 0, &printed(&cache))
                .with_exit(BUN, BUN_CLEAR, 0, ""),
        );
        let bun = BunCache::with_runner(runner.clone());
        let ctx = ScanContext::unchecked();
        let items = bun.discover(&ctx).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].path, cache);
        assert!(items[0].size >= 8192);
        assert!(ctx.take_findings().size_upper_bound.contains(&cache));
        assert!(!bun.desktop_trash_paths());
        assert_eq!(bun.desktop_tool(), Some(DesktopTool::Prune));
        assert!(bun.check_item(&cache).is_ok());
        assert!(bun.check_item(&dir.path().join("other")).is_err());
        assert_eq!(bun.run_desktop_tool(&item(BUN_ID, &cache)), Ok(()));
        let calls = runner.output_calls.lock().unwrap().clone();
        assert_eq!(calls.last().unwrap(), "bun pm cache rm");
        assert!(cache.join("wheels/w").exists(), "Bun decides what goes");
    }

    #[test]
    fn bun_lists_nothing_without_an_absolute_cache_and_offers_no_tool_without_bun() {
        let runner =
            MockRunner::new()
                .with_which(BUN)
                .with_exit(BUN, BUN_CACHE_DIR, 0, "relative\n");
        let bun = BunCache::with_runner(Arc::new(runner));
        assert!(bun.discover(&ScanContext::unchecked()).unwrap().is_empty());
        let missing = BunCache::with_runner(Arc::new(MockRunner::new()));
        assert!(!missing.available());
        assert_eq!(missing.desktop_tool(), None);
        assert_eq!(missing.quit_apps(), [BUN]);
    }

    #[test]
    fn pip_spawns_nothing_without_its_default_cache() {
        let home = tempfile::tempdir().unwrap();
        let runner = MockRunner::new().with_which(BREW_PIP);
        let items = pip_cache(
            &ScanContext::unchecked(),
            &runner,
            Some(BREW_PIP),
            home.path(),
        )
        .unwrap();
        assert!(items.is_empty());
        assert!(runner.output_calls.lock().unwrap().is_empty());
    }

    #[test]
    fn pip_lists_the_cache_a_real_pip_names_and_purges_it() {
        let home = tempfile::tempdir().unwrap();
        let cache = home.path().join(PIP_DEFAULT_DIR);
        cache_with_a_file(&cache);
        let runner = Arc::new(
            MockRunner::new()
                .with_which(BREW_PIP)
                .with_exit(BREW_PIP, PIP_CACHE_DIR, 0, &printed(&cache))
                .with_exit(BREW_PIP, PIP_PURGE, 0, ""),
        );
        let ctx = ScanContext::unchecked();
        let items = pip_cache(&ctx, runner.as_ref(), Some(BREW_PIP), home.path()).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].path, cache);
        assert!(ctx.take_findings().size_upper_bound.contains(&cache));
        let pip = PipCache::with_runner(runner.clone());
        assert_eq!(pip.desktop_tool(), Some(DesktopTool::Prune));
        assert!(pip.check_item(&cache).is_ok());
        assert_eq!(pip.run_desktop_tool(&item(PIP_ID, &cache)), Ok(()));
        let calls = runner.output_calls.lock().unwrap().clone();
        assert_eq!(calls.last().unwrap(), "/opt/homebrew/bin/pip3 cache purge");
    }

    #[test]
    fn pip_never_runs_the_system_shim_and_without_a_real_pip_stays_report_only() {
        let home = tempfile::tempdir().unwrap();
        let cache = home.path().join(PIP_DEFAULT_DIR);
        cache_with_a_file(&cache);
        // `pip3` on PATH is the shim; it is never asked anything.
        let runner = Arc::new(
            MockRunner::new()
                .with_which("pip3")
                .with_which("/usr/bin/pip3"),
        );
        let pip = PipCache::with_runner(runner.clone());
        assert_eq!(pip.desktop_tool(), None);
        let items = pip_cache(
            &ScanContext::unchecked(),
            runner.as_ref(),
            pip.pip(),
            home.path(),
        )
        .unwrap();
        assert_eq!(items[0].path, cache, "reported, never purged");
        assert!(pip.check_item(&cache).is_err());
        assert!(pip.run_desktop_tool(&item(PIP_ID, &cache)).is_err());
        assert!(runner.output_calls.lock().unwrap().is_empty());
        assert!(PIPS.iter().all(|p| !p.starts_with("/usr/bin")));
    }
}
