use crate::error::Result;
use std::path::PathBuf;
use std::sync::Arc;

use super::{execute_per_item, root_as_item, run_tool, CleanProvider};
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
        "pnpm content store pnpm re-downloads when needed".into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    fn desktop_trash_paths(&self) -> bool {
        true
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
        Ok(root_as_item(
            ctx,
            &path,
            PNPM_ID,
            PNPM_LABEL,
            RiskLevel::Review,
        ))
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_per_item(items, action, PNPM_ID)
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
