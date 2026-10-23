use crate::error::Result;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::tool_command::run_tool_command;
use super::{
    execute_per_item, execute_tool_command, root_as_item, run_tool, CleanProvider, DesktopTool,
};
use crate::clean::fs_safe::is_dir_safe;
use crate::clean::process::command_line_running;
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};
use crate::runner::CommandRunner;

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// Cache directory printed by a package manager. A tool that cannot run is
/// an error; a non-zero exit or empty output means "no cache".
fn tool_path(runner: &dyn CommandRunner, bin: &str, args: &[&str]) -> Result<Option<PathBuf>> {
    let output = run_tool(runner, bin, args)?;
    let s = output.stdout.trim();
    if !output.success() || s.is_empty() {
        return Ok(None);
    }
    Ok(Some(PathBuf::from(s)))
}

/// Refuses unless the tool still names `path` as its cache, so its prune
/// acts on the cache that was reviewed.
fn still_tool_path(
    runner: &dyn CommandRunner,
    bin: &str,
    args: &[&str],
    path: &Path,
) -> std::result::Result<(), String> {
    match tool_path(runner, bin, args) {
        Ok(Some(now)) if now == path => Ok(()),
        Ok(Some(now)) => Err(format!("{bin} now uses {}", now.display())),
        Ok(None) => Err(format!("{bin} {} printed no path", args.join(" "))),
        Err(e) => Err(e.to_string()),
    }
}

/// A cache that its tool prunes: listed with its full size, which the prune
/// frees at most, so the size is only an upper bound.
fn prunable_cache(
    ctx: &ScanContext<'_>,
    path: &Path,
    id: &str,
    label: &str,
    risk: RiskLevel,
) -> Vec<CleanItem> {
    if !is_dir_safe(path) {
        return Vec::new();
    }
    let items = root_as_item(ctx, path, id, label, risk);
    ctx.note_size_upper_bound(path);
    items
}

// ---------- Cargo ----------

const CARGO_ID: &str = "cargo";
const CARGO_LABEL: &str = "Cargo cache";

/// Cargo subdirs we will touch — pinned. Anything else under `~/.cargo`
/// (especially `bin/`, `config.toml`, `credentials*`) is off-limits.
const CARGO_SUBDIRS: &[&str] = &["registry/cache", "registry/src", "git/db", "git/checkouts"];

pub struct CargoCache;

impl CleanProvider for CargoCache {
    fn id(&self) -> &'static str {
        CARGO_ID
    }
    fn label(&self) -> &'static str {
        CARGO_LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Cargo registry and git caches Cargo re-downloads on the next build".into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    fn desktop_trash_paths(&self) -> bool {
        true
    }
    fn available(&self) -> bool {
        home().map(|h| h.join(".cargo").is_dir()).unwrap_or(false)
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let h = match home() {
            Some(h) => h,
            None => return Ok(Vec::new()),
        };
        let cargo = h.join(".cargo");
        let mut items = Vec::new();
        for sub in CARGO_SUBDIRS {
            let path = cargo.join(sub);
            items.extend(root_as_item(
                ctx,
                &path,
                CARGO_ID,
                CARGO_LABEL,
                RiskLevel::Review,
            ));
        }
        Ok(items)
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        let (safe, unsafe_items): (Vec<CleanItem>, Vec<CleanItem>) = items
            .iter()
            .cloned()
            .partition(|item| is_safe_cargo_path(&item.path));
        let mut report = execute_per_item(&safe, action, CARGO_ID)?;
        for item in unsafe_items {
            report
                .failed
                .push((item.path, "refused: not a pinned cargo cache dir".into()));
        }
        Ok(report)
    }
    fn check_item(&self, path: &std::path::Path) -> std::result::Result<(), String> {
        if is_safe_cargo_path(path) {
            Ok(())
        } else {
            Err("not a pinned cargo cache dir".into())
        }
    }
}

/// Guard: returned only true for paths under `~/.cargo/<one of CARGO_SUBDIRS>`.
/// Enforced by execute() and by the desktop's per-item check.
pub fn is_safe_cargo_path(path: &std::path::Path) -> bool {
    let h = match home() {
        Some(h) => h,
        None => return false,
    };
    let cargo = h.join(".cargo");
    CARGO_SUBDIRS
        .iter()
        .any(|sub| path == cargo.join(sub).as_path())
}

// ---------- npm ----------

const NPM_ID: &str = "npm";
const NPM_LABEL: &str = "npm cache";

pub struct NpmCache {
    runner: Arc<dyn CommandRunner>,
}

impl NpmCache {
    pub fn with_runner(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }
}

impl CleanProvider for NpmCache {
    fn id(&self) -> &'static str {
        NPM_ID
    }
    fn label(&self) -> &'static str {
        NPM_LABEL
    }
    fn inclusion_reason(&self) -> String {
        "npm package cache npm re-downloads when needed".into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    fn desktop_trash_paths(&self) -> bool {
        true
    }
    fn available(&self) -> bool {
        self.runner.which("npm")
    }
    fn required_tool(&self) -> Option<&'static str> {
        Some("npm")
    }
    fn roots_from_tool_output(&self) -> bool {
        true
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let path = match tool_path(self.runner.as_ref(), "npm", &["config", "get", "cache"])? {
            Some(p) => p,
            None => return Ok(Vec::new()),
        };
        Ok(root_as_item(
            ctx,
            &path,
            NPM_ID,
            NPM_LABEL,
            RiskLevel::Review,
        ))
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_per_item(items, action, NPM_ID)
    }
}

// ---------- pnpm ----------

const PNPM_ID: &str = "pnpm";
const PNPM_LABEL: &str = "pnpm store";
const PNPM_PRUNE: &[&str] = &["store", "prune"];

/// `pgrep -f` pattern for a running pnpm: the native binary, or the
/// `pnpm.cjs`/`pnpm.js` script Node runs for npm and Corepack installs, whose
/// process name is `node`. Anchored on a path component, so `pnpm-lock.yaml`
/// or the store path `…/pnpm/store` does not match.
const PNPM_COMMAND_LINE: &str = "(^|/)pnpm(\\.c?js)?( |$)";

pub struct PnpmStore {
    runner: Arc<dyn CommandRunner>,
}

impl PnpmStore {
    pub fn with_runner(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }
}

impl CleanProvider for PnpmStore {
    fn id(&self) -> &'static str {
        PNPM_ID
    }
    fn label(&self) -> &'static str {
        PNPM_LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Store packages no project references; pnpm store prune removes them and pnpm \
         re-downloads them when needed"
            .into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    /// The native binary; `check_item` also finds pnpm run by Node.
    fn requires_app_quit(&self) -> Option<&'static str> {
        Some("pnpm")
    }
    fn available(&self) -> bool {
        self.runner.which("pnpm")
    }
    fn required_tool(&self) -> Option<&'static str> {
        Some("pnpm")
    }
    fn roots_from_tool_output(&self) -> bool {
        true
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let path = match tool_path(self.runner.as_ref(), "pnpm", &["store", "path"])? {
            Some(p) => p,
            None => return Ok(Vec::new()),
        };
        Ok(prunable_cache(
            ctx,
            &path,
            PNPM_ID,
            PNPM_LABEL,
            RiskLevel::Review,
        ))
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_tool_command(
            self.runner.as_ref(),
            items,
            action,
            PNPM_ID,
            "pnpm",
            PNPM_PRUNE,
        )
    }
    fn desktop_tool(&self) -> Option<DesktopTool> {
        self.runner.which("pnpm").then_some(DesktopTool::Prune)
    }
    fn run_desktop_tool(&self, _item: &CleanItem) -> std::result::Result<(), String> {
        run_tool_command(self.runner.as_ref(), "pnpm", PNPM_PRUNE)
    }
    fn check_item(&self, path: &Path) -> std::result::Result<(), String> {
        match command_line_running(self.runner.as_ref(), PNPM_COMMAND_LINE) {
            Ok(false) => {}
            Ok(true) => return Err("pnpm is running".into()),
            Err(e) => return Err(format!("running-pnpm check failed: {e}")),
        }
        still_tool_path(self.runner.as_ref(), "pnpm", &["store", "path"], path)
    }
}

// ---------- yarn ----------

const YARN_ID: &str = "yarn";
const YARN_LABEL: &str = "yarn cache";

pub struct YarnCache {
    runner: Arc<dyn CommandRunner>,
}

impl YarnCache {
    pub fn with_runner(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }
}

impl CleanProvider for YarnCache {
    fn id(&self) -> &'static str {
        YARN_ID
    }
    fn label(&self) -> &'static str {
        YARN_LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Yarn package cache Yarn re-downloads when needed".into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    fn desktop_trash_paths(&self) -> bool {
        true
    }
    fn available(&self) -> bool {
        self.runner.which("yarn")
    }
    fn required_tool(&self) -> Option<&'static str> {
        Some("yarn")
    }
    fn roots_from_tool_output(&self) -> bool {
        true
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let path = match tool_path(self.runner.as_ref(), "yarn", &["cache", "dir"])? {
            Some(p) => p,
            None => return Ok(Vec::new()),
        };
        Ok(root_as_item(
            ctx,
            &path,
            YARN_ID,
            YARN_LABEL,
            RiskLevel::Review,
        ))
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_per_item(items, action, YARN_ID)
    }
}

// ---------- uv ----------

const UV_ID: &str = "uv";
const UV_LABEL: &str = "uv cache";
const UV_CACHE_DIR: &[&str] = &["cache", "dir"];
const UV_PRUNE: &[&str] = &["cache", "prune"];
/// uv's cache when uv is not installed, so the configured one is unknown.
const UV_DEFAULT_DIR: &str = ".cache/uv";

pub struct UvCache {
    runner: Arc<dyn CommandRunner>,
}

impl UvCache {
    pub fn with_runner(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }
}

impl CleanProvider for UvCache {
    fn id(&self) -> &'static str {
        UV_ID
    }
    fn label(&self) -> &'static str {
        UV_LABEL
    }
    fn inclusion_reason(&self) -> String {
        "uv cache entries nothing uses any more; uv cache prune removes them".into()
    }
    /// Review although a prune removes only unreferenced entries: it is
    /// permanent whichever action is chosen, so `tiny clean` must not tick it
    /// under "Move to Trash (recoverable)".
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    fn quit_apps(&self) -> Vec<&'static str> {
        vec!["uv", "uvx"]
    }
    /// Also true without uv when its default cache exists, so it is still
    /// reported; it cannot be pruned then.
    fn available(&self) -> bool {
        self.runner.which("uv") || home().is_some_and(|h| is_dir_safe(&h.join(UV_DEFAULT_DIR)))
    }
    fn required_tool(&self) -> Option<&'static str> {
        Some("uv")
    }
    fn roots_from_tool_output(&self) -> bool {
        true
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let Some(h) = home() else {
            return Ok(Vec::new());
        };
        uv_cache(ctx, self.runner.as_ref(), &h)
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_tool_command(self.runner.as_ref(), items, action, UV_ID, "uv", UV_PRUNE)
    }
    /// Never without uv: its default cache alone stays report only.
    fn desktop_tool(&self) -> Option<DesktopTool> {
        self.runner.which("uv").then_some(DesktopTool::Prune)
    }
    fn run_desktop_tool(&self, _item: &CleanItem) -> std::result::Result<(), String> {
        run_tool_command(self.runner.as_ref(), "uv", UV_PRUNE)
    }
    fn check_item(&self, path: &Path) -> std::result::Result<(), String> {
        still_tool_path(self.runner.as_ref(), "uv", UV_CACHE_DIR, path)
    }
}

/// The cache `uv cache dir` names, which must be absolute, or the default
/// cache under `home` when uv is not installed. A uv that answers anything
/// else lists nothing: its configured cache is unknown.
fn uv_cache(
    ctx: &ScanContext<'_>,
    runner: &dyn CommandRunner,
    home: &Path,
) -> Result<Vec<CleanItem>> {
    let path = if runner.which("uv") {
        match tool_path(runner, "uv", UV_CACHE_DIR)? {
            Some(path) if path.is_absolute() => path,
            _ => return Ok(Vec::new()),
        }
    } else {
        home.join(UV_DEFAULT_DIR)
    };
    Ok(prunable_cache(
        ctx,
        &path,
        UV_ID,
        UV_LABEL,
        RiskLevel::Review,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::test_support::MockRunner;

    #[test]
    fn cargo_safe_path_accepts_pinned_subdirs() {
        let h = match home() {
            Some(h) => h,
            None => return,
        };
        for sub in CARGO_SUBDIRS {
            assert!(is_safe_cargo_path(&h.join(".cargo").join(sub)));
        }
    }

    #[test]
    fn cargo_safe_path_rejects_dangerous_paths() {
        let h = match home() {
            Some(h) => h,
            None => return,
        };
        assert!(!is_safe_cargo_path(&h.join(".cargo/bin")));
        assert!(!is_safe_cargo_path(&h.join(".cargo/config.toml")));
        assert!(!is_safe_cargo_path(&h.join(".cargo/credentials")));
        assert!(!is_safe_cargo_path(&h.join(".cargo/credentials.toml")));
        assert!(!is_safe_cargo_path(&h.join(".rustup")));
        assert!(!is_safe_cargo_path(&h.join(".cargo")));
    }

    const PGREP: &str = "/usr/bin/pgrep";

    fn cache_with_a_file(root: &Path) -> PathBuf {
        let cache = root.join("cache");
        std::fs::create_dir_all(cache.join("entries")).unwrap();
        std::fs::write(cache.join("entries/blob"), vec![1u8; 8192]).unwrap();
        cache
    }

    fn printed(path: &Path) -> String {
        format!("{}\n", path.display())
    }

    fn paths(items: &[CleanItem]) -> Vec<PathBuf> {
        items.iter().map(|i| i.path.clone()).collect()
    }

    fn calls(runner: &MockRunner) -> Vec<String> {
        runner.run_calls.lock().unwrap().clone()
    }

    // ---------- uv ----------

    #[test]
    fn uv_lists_the_cache_uv_names_as_an_upper_bound() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_with_a_file(dir.path());
        let runner =
            MockRunner::new()
                .with_which("uv")
                .with_exit("uv", UV_CACHE_DIR, 0, &printed(&cache));
        let ctx = ScanContext::unchecked();
        let items = uv_cache(&ctx, &runner, dir.path()).unwrap();
        assert_eq!(paths(&items), vec![cache.clone()]);
        assert_eq!(items[0].risk, RiskLevel::Review);
        assert_eq!(
            UvCache::with_runner(Arc::new(runner)).risk(),
            RiskLevel::Review
        );
        assert!(items[0].size >= 8192);
        // `uv cache prune` removes only what nothing uses, so at most this.
        assert!(ctx.take_findings().size_upper_bound.contains(&cache));
    }

    #[test]
    fn uv_lists_nothing_when_it_names_no_absolute_cache() {
        let dir = tempfile::tempdir().unwrap();
        // The default exists but is not a fallback: uv may use another.
        std::fs::create_dir_all(dir.path().join(UV_DEFAULT_DIR)).unwrap();
        // `src` exists relative to the test's working directory.
        for (status, stdout) in [(0, "src\n"), (0, ""), (2, "/abs/uv\n")] {
            let runner =
                MockRunner::new()
                    .with_which("uv")
                    .with_exit("uv", UV_CACHE_DIR, status, stdout);
            let items = uv_cache(&ScanContext::unchecked(), &runner, dir.path()).unwrap();
            assert!(items.is_empty(), "{status} {stdout:?}");
        }
    }

    #[test]
    fn without_uv_its_default_cache_is_reported_but_never_pruned() {
        let dir = tempfile::tempdir().unwrap();
        let default = dir.path().join(UV_DEFAULT_DIR);
        std::fs::create_dir_all(&default).unwrap();
        let runner = Arc::new(MockRunner::new());
        let items = uv_cache(&ScanContext::unchecked(), runner.as_ref(), dir.path()).unwrap();
        assert_eq!(paths(&items), vec![default.clone()]);
        let uv = UvCache::with_runner(runner.clone());
        assert!(uv.check_item(&default).is_err());
        let report = uv.execute(&items, ExecAction::HardDelete).unwrap();
        assert!(report.removed_paths.is_empty());
        assert_eq!(report.failed[0].1, "uv was not found; nothing was removed");
        assert!(calls(&runner).is_empty());
        assert!(default.exists());
    }

    #[test]
    fn uv_is_skipped_while_uv_or_uvx_runs() {
        use crate::clean::discover::{discover_checked, CategoryOutcome};
        use crate::clean::process::test_support::MockChecker;
        let runner = Arc::new(MockRunner::new().with_which("uv"));
        let providers: Vec<Box<dyn CleanProvider>> =
            vec![Box::new(UvCache::with_runner(runner.clone()))];
        let checker = MockChecker::with_running(["uvx"]);
        let found = discover_checked(&providers, &ScanContext::new(None, &checker), None);
        assert!(matches!(
            &found.categories[0].outcome,
            CategoryOutcome::AppRunning { app } if app == "uvx"
        ));
        assert!(runner.output_calls.lock().unwrap().is_empty());
        // The executors re-check the same names right before pruning.
        assert_eq!(providers[0].item_apps(Path::new("/x")), ["uv", "uvx"]);
    }

    #[test]
    fn uv_prunes_with_its_own_command_only_the_reviewed_cache() {
        let dir = tempfile::tempdir().unwrap();
        let cache = cache_with_a_file(dir.path());
        let runner = Arc::new(
            MockRunner::new()
                .with_which("uv")
                .with_exit("uv", UV_CACHE_DIR, 0, &printed(&cache))
                .with_response("uv", UV_PRUNE, true, ""),
        );
        let uv = UvCache::with_runner(runner.clone());
        assert!(!uv.desktop_trash_paths());
        assert!(uv.check_item(&cache).is_ok());
        assert_eq!(
            uv.check_item(&dir.path().join("elsewhere")),
            Err(format!("uv now uses {}", cache.display()))
        );
        let item = CleanItem {
            category_id: UV_ID.into(),
            category_label: UV_LABEL.into(),
            path: cache.clone(),
            size: 1,
            risk: RiskLevel::Safe,
            evidence: Vec::new(),
        };
        assert!(uv
            .execute(std::slice::from_ref(&item), ExecAction::EmptyTrash)
            .is_err());
        let report = uv.execute(&[item], ExecAction::Trash).unwrap();
        assert_eq!(report.removed_paths, vec![cache.clone()]);
        assert_eq!(calls(&runner), ["uv cache prune"]);
        assert!(cache.exists(), "uv decides what to remove");
    }

    #[test]
    fn the_desktop_prunes_with_uv_or_pnpm_bounded_and_only_when_found() {
        let item = |id: &str| CleanItem {
            category_id: id.into(),
            category_label: id.into(),
            path: PathBuf::from("/reviewed/cache"),
            size: 1,
            risk: RiskLevel::Safe,
            evidence: Vec::new(),
        };
        let missing: Arc<dyn CommandRunner> = Arc::new(MockRunner::new());
        assert_eq!(UvCache::with_runner(missing.clone()).desktop_tool(), None);
        assert_eq!(PnpmStore::with_runner(missing).desktop_tool(), None);

        let runner = Arc::new(
            MockRunner::new()
                .with_which("uv")
                .with_which("pnpm")
                .with_exit("uv", UV_PRUNE, 0, "")
                .with_exit("pnpm", PNPM_PRUNE, 1, ""),
        );
        let uv = UvCache::with_runner(runner.clone());
        let pnpm = PnpmStore::with_runner(runner.clone());
        assert_eq!(uv.desktop_tool(), Some(DesktopTool::Prune));
        assert_eq!(pnpm.desktop_tool(), Some(DesktopTool::Prune));
        assert_eq!(uv.run_desktop_tool(&item(UV_ID)), Ok(()));
        assert!(pnpm.run_desktop_tool(&item(PNPM_ID)).is_err());
        let bounded = runner.output_calls.lock().unwrap().clone();
        assert_eq!(bounded, ["uv cache prune", "pnpm store prune"]);
        assert!(calls(&runner).is_empty(), "never the CLI's unbounded run");
    }

    // ---------- pnpm ----------

    fn pnpm_runner(store: &Path, pgrep_status: i32) -> MockRunner {
        MockRunner::new()
            .with_which("pnpm")
            .with_exit("pnpm", &["store", "path"], 0, &printed(store))
            .with_exit(PGREP, &["-f", "--", PNPM_COMMAND_LINE], pgrep_status, "")
    }

    #[test]
    fn pnpm_store_is_pruned_by_pnpm_never_moved() {
        let dir = tempfile::tempdir().unwrap();
        let store = cache_with_a_file(dir.path());
        let runner = Arc::new(pnpm_runner(&store, 1).with_response("pnpm", PNPM_PRUNE, true, ""));
        let pnpm = PnpmStore::with_runner(runner.clone());
        assert!(!pnpm.desktop_trash_paths());
        let ctx = ScanContext::unchecked();
        let items = pnpm.discover(&ctx).unwrap();
        assert_eq!(paths(&items), vec![store.clone()]);
        assert!(ctx.take_findings().size_upper_bound.contains(&store));
        assert!(pnpm.check_item(&store).is_ok());
        let report = pnpm.execute(&items, ExecAction::Trash).unwrap();
        assert_eq!(report.removed_paths, vec![store.clone()]);
        assert!(report.failed.is_empty());
        assert_eq!(calls(&runner), ["pnpm store prune"]);
        assert!(
            store.join("entries/blob").exists(),
            "nothing moved or deleted"
        );
    }

    #[test]
    fn pnpm_is_refused_while_it_runs_or_when_that_is_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let store = cache_with_a_file(dir.path());
        // Native pnpm is a process name; pnpm run by Node is only on the command line.
        let native = PnpmStore::with_runner(Arc::new(pnpm_runner(&store, 1)));
        assert_eq!(native.quit_apps(), ["pnpm"]);
        let running = PnpmStore::with_runner(Arc::new(pnpm_runner(&store, 0)));
        assert_eq!(running.check_item(&store), Err("pnpm is running".into()));
        let unknown = PnpmStore::with_runner(Arc::new(pnpm_runner(&store, 2)));
        assert!(unknown
            .check_item(&store)
            .unwrap_err()
            .starts_with("running-pnpm check failed"));
        let failed = Arc::new(MockRunner::new().with_which("pnpm").with_output(
            PGREP,
            &["-f", "--", PNPM_COMMAND_LINE],
            Err(crate::runner::CommandError::Timeout {
                bin: PGREP.into(),
                timeout_ms: 5000,
            }),
        ));
        assert!(PnpmStore::with_runner(failed.clone())
            .check_item(&store)
            .is_err());
        assert!(calls(&failed).is_empty());
    }

    #[test]
    fn pnpm_prune_is_refused_when_pnpm_now_uses_another_store() {
        let dir = tempfile::tempdir().unwrap();
        let reviewed = cache_with_a_file(dir.path());
        let runner = Arc::new(pnpm_runner(&dir.path().join("other"), 1));
        let pnpm = PnpmStore::with_runner(runner.clone());
        assert!(pnpm
            .check_item(&reviewed)
            .unwrap_err()
            .starts_with("pnpm now uses"));
        assert!(calls(&runner).is_empty());
    }

    #[test]
    fn pnpm_command_line_pattern_matches_pnpm_and_nothing_near_it() {
        use std::io::Write;
        // pgrep -f takes an extended regex; grep -E applies the same syntax.
        let matches = |line: &str| {
            let mut grep = std::process::Command::new("/usr/bin/grep")
                .args(["-Eq", "--", PNPM_COMMAND_LINE])
                .stdin(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            grep.stdin
                .take()
                .unwrap()
                .write_all(line.as_bytes())
                .unwrap();
            grep.wait().unwrap().success()
        };
        for pnpm in [
            "pnpm install",
            "/opt/homebrew/bin/pnpm add left-pad",
            "/usr/local/bin/pnpm",
            "node /Users/a/.nvm/versions/node/v24/lib/node_modules/corepack/dist/pnpm.js i",
            "node /usr/local/lib/node_modules/pnpm/bin/pnpm.cjs store prune",
        ] {
            assert!(matches(pnpm), "{pnpm}");
        }
        for other in [
            "vim pnpm-lock.yaml",
            "ls /Users/a/Library/pnpm/store/v11",
            "node /a/node_modules/.pnpm/vite@5/node_modules/vite/bin/vite.js",
            "cat notpnpm",
            "zsh -c pnpmfile",
        ] {
            assert!(!matches(other), "{other}");
        }
    }
}
