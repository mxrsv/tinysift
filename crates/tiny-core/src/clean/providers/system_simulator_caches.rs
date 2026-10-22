//! Caches CoreSimulator keeps for every account in
//! `/Library/Developer/CoreSimulator/Caches`, mostly `dyld/`: the
//! runtimes' shared caches, built again when a simulator boots. Root owns
//! them, so removing them needs administrator rights (`needs_admin`): the
//! desktop only reports them, and the CLI, which never runs `sudo`, refuses
//! them.
//!
//! Refused while Xcode, Simulator or a build or test tool runs and, when
//! `simctl` exists, while any simulator is booted. Never gated on
//! `simdiskimaged`, `CoreSimulatorService` or `SimLaunchHost`: they run
//! without Xcode, so the caches would never be offered.

use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use crate::engine_error;
use crate::error::Result;

use super::simulator_devices::{simctl_found, XCRUN};
use super::{run_tool, CleanProvider, NEEDS_ADMIN};
use crate::clean::checked_execute::ancestors_are_real_dirs;
use crate::clean::fs_safe::{dir_size_checked, is_dir_safe, list_children};
use crate::clean::scan_context::ScanContext;
use crate::clean::trash_plan::inside_system;
use crate::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};
use crate::runner::CommandRunner;

const ID: &str = "system-simulator-caches";
const LABEL: &str = "System simulator caches";

/// The only folder whose entries this category offers, and the allowlist
/// elevated deletion checks against (`check_system_cache_path`).
pub const CACHES_ROOT: &str = "/Library/Developer/CoreSimulator/Caches";

/// Processes that use or rebuild the caches (`pgrep -x` names). `XCTRunner`
/// hosts UI tests, as Mole also guards.
pub const SIMULATOR_ACTIVITY: &[&str] = &[
    "Xcode",
    "Simulator",
    "xcodebuild",
    "xctest",
    "XCTRunner",
    "simctl",
];

const BOOTED_ARGS: &[&str] = &["simctl", "list", "devices", "booted", "-j"];

/// `simctl list devices booted -j`: runtime id -> booted devices.
#[derive(serde::Deserialize)]
struct BootedList {
    devices: BTreeMap<String, Vec<BootedDevice>>,
}

#[derive(serde::Deserialize)]
struct BootedDevice {
    udid: String,
    name: String,
}

pub struct SystemSimulatorCaches {
    root: PathBuf,
    runner: Arc<dyn CommandRunner>,
}

impl SystemSimulatorCaches {
    pub fn with_runner(runner: Arc<dyn CommandRunner>) -> Self {
        Self::with_root(PathBuf::from(CACHES_ROOT), runner)
    }

    /// Lists `root` instead of `CACHES_ROOT`, for tests. Elevated deletion
    /// never uses the provider's root: it checks `CACHES_ROOT` itself.
    pub fn with_root(root: PathBuf, runner: Arc<dyn CommandRunner>) -> Self {
        Self { root, runner }
    }

    /// Why the caches must wait for simulators, or `None` when none is
    /// booted. Without `simctl` nothing can boot one. A failed or unexpected
    /// answer refuses: deleting on a guess is never safe.
    fn booted_refusal(&self) -> Option<String> {
        let runner = self.runner.as_ref();
        if !simctl_found(runner) {
            return None;
        }
        let unknown =
            |detail: String| Some(format!("could not check for booted simulators: {detail}"));
        let out = match run_tool(runner, XCRUN, BOOTED_ARGS) {
            Ok(out) if out.success() => out,
            Ok(out) => return unknown(out.stderr.trim().to_string()),
            Err(e) => return unknown(e.to_string()),
        };
        match serde_json::from_str::<BootedList>(&out.stdout) {
            Ok(list) => list
                .devices
                .into_values()
                .flatten()
                .next()
                .map(|d| format!("simulator {} ({}) is booted", d.name, d.udid)),
            Err(e) => unknown(format!("unexpected xcrun simctl output: {e}")),
        }
    }
}

impl CleanProvider for SystemSimulatorCaches {
    fn id(&self) -> &'static str {
        ID
    }
    fn label(&self) -> &'static str {
        LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Caches CoreSimulator keeps for all accounts in /Library/Developer/CoreSimulator/Caches and builds again when a simulator boots"
            .into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Safe
    }
    fn quit_apps(&self) -> Vec<&'static str> {
        SIMULATOR_ACTIVITY.to_vec()
    }
    fn needs_admin(&self) -> bool {
        true
    }
    fn available(&self) -> bool {
        is_dir_safe(&self.root)
    }
    /// A booted simulator, or an unknown answer, refuses every entry with
    /// the reason instead of offering any.
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let entries = list_children(&self.root, ctx);
        if entries.is_empty() {
            return Ok(Vec::new());
        }
        if let Some(reason) = self.booted_refusal() {
            for path in &entries {
                ctx.note_refused(path, reason.clone());
            }
            return Ok(Vec::new());
        }
        let mut items = Vec::new();
        for path in entries {
            if let Err(reason) = check_entry(&self.root, &path) {
                ctx.note_refused(&path, reason);
                continue;
            }
            items.push(CleanItem {
                category_id: ID.to_string(),
                category_label: LABEL.to_string(),
                size: dir_size_checked(&path, ctx),
                path,
                risk: RiskLevel::Safe,
                evidence: Vec::new(),
            });
        }
        Ok(items)
    }
    fn check_item(&self, path: &Path) -> std::result::Result<(), String> {
        check_entry(&self.root, path)?;
        self.booted_refusal().map_or(Ok(()), Err)
    }
    /// Refuses every item: removing them needs administrator rights, and
    /// Finder would ask for them itself.
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        if matches!(action, ExecAction::EmptyTrash) {
            return Err(engine_error!("{} provider does not accept EmptyTrash", ID));
        }
        Ok(ExecReport {
            failed: items
                .iter()
                .map(|item| (item.path.clone(), NEEDS_ADMIN.to_string()))
                .collect(),
            ..ExecReport::default()
        })
    }
}

/// Why `path` must not be removed as a system simulator cache, against the
/// fixed `CACHES_ROOT`. Elevated deletion runs this for every path right
/// before it acts.
pub fn check_system_cache_path(path: &Path) -> std::result::Result<(), String> {
    check_entry(Path::new(CACHES_ROOT), path)
}

/// `path` must be absolute, outside `/System`, named in UTF-8 without NUL,
/// directly inside `root`, a real file or folder (never a symlink), with
/// only real folders above it.
fn check_entry(root: &Path, path: &Path) -> std::result::Result<(), String> {
    if !path.is_absolute() {
        return Err("not an absolute path".into());
    }
    if path.components().any(|c| c == Component::ParentDir) {
        return Err("the path holds '..'".into());
    }
    if inside_system(path) {
        return Err("inside /System".into());
    }
    if !path.to_str().is_some_and(|name| !name.contains('\0')) {
        return Err("the path is not UTF-8 or holds NUL".into());
    }
    if path.parent() != Some(root) || path.file_name().is_none() {
        return Err(format!("not directly inside {}", root.display()));
    }
    let meta = std::fs::symlink_metadata(path).map_err(|e| e.to_string())?;
    let kind = meta.file_type();
    if kind.is_symlink() {
        return Err("a symbolic link".into());
    }
    if !kind.is_dir() && !kind.is_file() {
        return Err("not a file or folder".into());
    }
    if !ancestors_are_real_dirs(path) {
        return Err("a folder above it is a symbolic link".into());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::providers::simulator_devices::FIND_ARGS;
    use crate::clean::providers::{desktop_report_only, known_category_ids, ReportOnly};
    use crate::runner::test_support::MockRunner;
    use std::os::unix::fs::MetadataExt;

    const BOOTED: &str = r#"{"devices": {
  "com.apple.CoreSimulator.SimRuntime.iOS-26-0": [
    {"udid": "6F1C0E52-7A0B-4C1D-9E2F-3A4B5C6D7E8F", "name": "iPhone 17", "state": "Booted"}
  ],
  "com.apple.CoreSimulator.SimRuntime.watchOS-26-0": []
}}"#;
    const NONE_BOOTED: &str = r#"{"devices": {
  "com.apple.CoreSimulator.SimRuntime.iOS-26-0": []
}}"#;

    /// A physical temp root (not macOS's `/var` alias) holding `dyld/` with
    /// one file, a cache file and a symlink.
    fn caches() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap().join("Caches");
        std::fs::create_dir_all(root.join("dyld/23A8464")).unwrap();
        std::fs::write(root.join("dyld/23A8464/cache"), vec![7u8; 9000]).unwrap();
        std::fs::write(root.join("index.db"), b"12345").unwrap();
        std::os::unix::fs::symlink(dir.path(), root.join("link")).unwrap();
        (dir, root)
    }

    fn no_simctl() -> MockRunner {
        MockRunner::new().with_exit(XCRUN, FIND_ARGS, 72, "")
    }

    fn with_simctl(booted: std::result::Result<&str, i32>) -> MockRunner {
        let runner = MockRunner::new().with_exit(XCRUN, FIND_ARGS, 0, "/usr/bin/simctl\n");
        match booted {
            Ok(json) => runner.with_exit(XCRUN, BOOTED_ARGS, 0, json),
            Err(status) => runner.with_exit(XCRUN, BOOTED_ARGS, status, ""),
        }
    }

    fn provider(root: &Path, runner: MockRunner) -> (SystemSimulatorCaches, Arc<MockRunner>) {
        let runner = Arc::new(runner);
        let p = SystemSimulatorCaches::with_root(root.to_path_buf(), runner.clone());
        (p, runner)
    }

    fn allocated(path: &Path) -> u64 {
        std::fs::symlink_metadata(path).unwrap().blocks() * 512
    }

    fn item(path: &Path) -> CleanItem {
        CleanItem {
            category_id: ID.into(),
            category_label: LABEL.into(),
            path: path.to_path_buf(),
            size: 0,
            risk: RiskLevel::Safe,
            evidence: Vec::new(),
        }
    }

    #[test]
    fn offers_real_entries_directly_inside_the_caches_and_refuses_symlinks() {
        let (_dir, root) = caches();
        let (p, runner) = provider(&root, no_simctl());
        let ctx = ScanContext::unchecked();
        let mut items = p.discover(&ctx).unwrap();
        items.sort_by(|a, b| a.path.cmp(&b.path));
        let paths: Vec<_> = items.iter().map(|i| i.path.clone()).collect();
        assert_eq!(paths, vec![root.join("dyld"), root.join("index.db")]);
        assert_eq!(items[0].size, allocated(&root.join("dyld/23A8464/cache")));
        assert!(items.iter().all(|i| i.risk == RiskLevel::Safe));
        let refused = ctx.take_findings().refused;
        assert_eq!(refused.keys().collect::<Vec<_>>(), vec![&root.join("link")]);
        assert!(refused[&root.join("link")].contains("symbolic link"));
        let calls = runner.output_calls.lock().unwrap().clone();
        assert!(
            calls.iter().all(|c| !c.contains("booted")),
            "without simctl nothing can boot a simulator: {calls:?}"
        );
    }

    #[test]
    fn a_booted_simulator_refuses_every_entry() {
        let (_dir, root) = caches();
        let (p, _) = provider(&root, with_simctl(Ok(BOOTED)));
        let ctx = ScanContext::unchecked();
        assert!(p.discover(&ctx).unwrap().is_empty());
        let refused = ctx.take_findings().refused;
        assert!(refused.contains_key(&root.join("dyld")));
        assert!(
            refused[&root.join("dyld")].contains("iPhone 17"),
            "{refused:?}"
        );
        assert!(p.check_item(&root.join("dyld")).is_err());
    }

    #[test]
    fn an_unknown_booted_state_refuses_every_entry() {
        let (_dir, root) = caches();
        for runner in [with_simctl(Err(1)), with_simctl(Ok("== Devices =="))] {
            let (p, _) = provider(&root, runner);
            let ctx = ScanContext::unchecked();
            assert!(p.discover(&ctx).unwrap().is_empty());
            let refused = ctx.take_findings().refused;
            assert!(refused.contains_key(&root.join("dyld")), "{refused:?}");
            assert!(p.check_item(&root.join("dyld")).is_err());
        }
    }

    #[test]
    fn no_booted_simulator_offers_the_caches() {
        let (_dir, root) = caches();
        let (p, _) = provider(&root, with_simctl(Ok(NONE_BOOTED)));
        let ctx = ScanContext::unchecked();
        assert_eq!(p.discover(&ctx).unwrap().len(), 2);
        assert!(p.check_item(&root.join("dyld")).is_ok());
    }

    #[test]
    fn only_real_direct_children_of_the_root_pass_the_path_check() {
        let (dir, root) = caches();
        assert!(check_entry(&root, &root.join("dyld")).is_ok());
        assert!(check_entry(&root, &root.join("index.db")).is_ok());
        let refused = [
            root.join("dyld/23A8464"),
            root.clone(),
            root.join("link"),
            root.join("missing"),
            root.join("../Caches/dyld"),
            root.join("dyld/.."),
            PathBuf::from("Caches/dyld"),
            PathBuf::from("/System/Library/Caches/dyld"),
        ];
        for path in refused {
            assert!(check_entry(&root, &path).is_err(), "{}", path.display());
        }
        // A symlinked folder above the entry redirects it elsewhere.
        let alias = std::fs::canonicalize(dir.path()).unwrap().join("alias");
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        assert!(check_entry(&alias, &alias.join("dyld")).is_err());
    }

    #[test]
    fn the_exported_check_allows_only_the_fixed_caches_folder() {
        let (_dir, root) = caches();
        assert!(check_system_cache_path(&root.join("dyld")).is_err());
        for path in [
            "/Library/Developer/CoreSimulator/Caches",
            "/Library/Developer/CoreSimulator/Caches/../Images",
            "/Library/Developer/CoreSimulator/Caches/../../../../System/Library",
            "/System/Library/AssetsV2/x.dmg",
            "/Library/Developer/CoreSimulator/Caches/tiny-test-missing",
        ] {
            assert!(check_system_cache_path(Path::new(path)).is_err(), "{path}");
        }
    }

    #[test]
    fn execute_refuses_every_item_and_touches_nothing() {
        let (_dir, root) = caches();
        let (p, runner) = provider(&root, no_simctl());
        for action in [ExecAction::Trash, ExecAction::HardDelete] {
            let report = p.execute(&[item(&root.join("dyld"))], action).unwrap();
            assert!(report.removed_paths.is_empty());
            assert_eq!(
                report.failed,
                vec![(root.join("dyld"), NEEDS_ADMIN.to_string())]
            );
        }
        assert!(p
            .execute(&[item(&root.join("dyld"))], ExecAction::EmptyTrash)
            .is_err());
        assert!(is_dir_safe(&root.join("dyld/23A8464")));
        assert!(runner.run_calls.lock().unwrap().is_empty());
    }

    #[test]
    fn registered_safe_needs_admin_and_gated_on_simulator_activity_only() {
        let p = SystemSimulatorCaches::with_runner(Arc::new(MockRunner::new()));
        assert!(known_category_ids().contains(&ID));
        assert_eq!(p.risk(), RiskLevel::Safe);
        assert!(p.needs_admin());
        assert_eq!(
            desktop_report_only(&p),
            Some(ReportOnly::NeedsAdministrator)
        );
        let apps = p.quit_apps();
        for app in ["Xcode", "Simulator", "xcodebuild", "xctest", "simctl"] {
            assert!(apps.contains(&app), "{app}");
        }
        for service in ["simdiskimaged", "CoreSimulatorService", "SimLaunchHost"] {
            assert!(!apps.contains(&service), "{service} runs without Xcode");
        }
        assert_eq!(p.item_apps(Path::new(CACHES_ROOT)), apps);
    }
}
