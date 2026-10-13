use crate::engine_error;
use crate::error::{Context, Result};
use crate::runner::{CommandOutcome, CommandRunner, RealRunner, TOOL_TIMEOUT};
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

use super::fs_safe::{dir_size_checked, list_children, remove_recursive_safe};
use super::scan_context::ScanContext;
use super::types::{CleanItem, ComesBack, ExecAction, ExecReport, RiskLevel};

pub mod android_sdk;
pub mod app_orphans;
pub mod browser_caches;
pub mod chat_caches;
pub mod crash_reports;
pub mod dev_caches;
pub mod docker;
pub mod downloads_old;
pub mod font_quicklook_caches;
pub mod go_cache;
pub mod gradle_maven;
pub mod ios_simulators;
pub mod jetbrains;
pub mod mail_attachments;
pub mod node_modules;
pub mod project_activity;
pub mod python_caches;
pub mod quarantine;
pub mod rust_targets;
pub mod screenshots_old;
pub mod simulator_devices;
pub mod streaming_caches;
pub mod time_machine_local;
pub mod trash;
pub mod user_caches;
pub mod user_logs;
pub mod vscode;
pub mod xcode;

/// Top-level grouping for the hierarchical picker. Source of truth lives in
/// `category_family()` below — providers must not declare their own family.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Family {
    Dev,
    UserStorage,
    System,
}

impl Family {
    #[allow(dead_code)]
    pub fn id(&self) -> &'static str {
        match self {
            Family::Dev => "dev",
            Family::UserStorage => "user-storage",
            Family::System => "system",
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Family::Dev => "Dev caches",
            Family::UserStorage => "User storage",
            Family::System => "System leftovers",
        }
    }
}

/// Maps a category id to its family. Panics on unknown id — every id in
/// `known_category_ids()` MUST be covered (verified by test below).
pub fn category_family(category_id: &str) -> Family {
    match category_id {
        // Dev family
        "cargo" | "npm" | "pnpm" | "yarn" => Family::Dev,
        "node-modules" | "python-caches" | "rust-targets" => Family::Dev,
        "gradle-maven" | "jetbrains" | "vscode" => Family::Dev,
        "ios-simulators" | "simulator-devices" | "android-sdk" => Family::Dev,
        "go-cache" | "docker" | "docker-volumes" => Family::Dev,
        "xcode-derived" | "xcode-archives" | "xcode-devicesupport" => Family::Dev,
        // UserStorage family
        "downloads-old" | "screenshots-old" | "mail-attachments" => Family::UserStorage,
        "streaming-caches" | "chat-caches" | "browser-caches" => Family::UserStorage,
        // System family
        "user-logs" | "user-caches" | "trash" => Family::System,
        "quarantine" | "crash-reports" | "font-quicklook-caches" => Family::System,
        "app-orphans" | "time-machine-local" => Family::System,
        other => panic!("unknown category id: {}", other),
    }
}

/// How a category's items come back after a move to Trash. `None` for an
/// id outside `known_category_ids()`. "Your files" on the desktop are the
/// `TrashOnly` categories.
pub fn comes_back(category_id: &str) -> Option<ComesBack> {
    let rebuild = |command| Some(ComesBack::Rebuild { command });
    match category_id {
        "cargo" | "npm" | "pnpm" | "yarn" | "gradle-maven" | "android-sdk" | "docker" => {
            Some(ComesBack::Redownload)
        }
        "streaming-caches" | "chat-caches" => Some(ComesBack::Redownload),
        "node-modules" => rebuild("npm, pnpm or yarn install"),
        "python-caches" => rebuild("python -m venv, then pip install"),
        "rust-targets" => rebuild("cargo build"),
        "go-cache" => rebuild("go build"),
        "xcode-derived" => rebuild("Build in Xcode"),
        "user-logs" | "user-caches" | "jetbrains" | "vscode" | "ios-simulators" => {
            Some(ComesBack::AppRecreates)
        }
        "xcode-devicesupport" | "browser-caches" | "quarantine" | "crash-reports" => {
            Some(ComesBack::AppRecreates)
        }
        "font-quicklook-caches" => Some(ComesBack::AppRecreates),
        "xcode-archives" | "downloads-old" | "screenshots-old" | "mail-attachments" => {
            Some(ComesBack::TrashOnly)
        }
        "app-orphans" => Some(ComesBack::TrashOnly),
        "time-machine-local" | "trash" | "docker-volumes" | "simulator-devices" => {
            Some(ComesBack::NotRecoverable)
        }
        _ => None,
    }
}

pub trait CleanProvider {
    fn id(&self) -> &'static str;
    fn label(&self) -> &'static str;
    /// One English sentence on why this category's items are candidates,
    /// shown beside them on the desktop (PC-C1). Not printed by the CLI.
    fn inclusion_reason(&self) -> String;
    fn risk(&self) -> RiskLevel;

    /// Process name that should NOT be running before discover/execute.
    /// `None` means no app gating.
    fn requires_app_quit(&self) -> Option<&'static str> {
        None
    }

    /// Every process name that must NOT be running before discover/execute:
    /// `requires_app_quit`, unless the provider gates on more than one app.
    fn quit_apps(&self) -> Vec<&'static str> {
        self.requires_app_quit().into_iter().collect()
    }

    /// Returns false when the provider is fundamentally unavailable on this
    /// system (e.g. CLI not installed). A provider that returns true here may
    /// still legitimately discover zero items.
    fn available(&self) -> bool {
        true
    }

    /// Tool whose absence makes `available()` false. Checked discovery
    /// reports such a provider as unavailable instead of omitting it.
    fn required_tool(&self) -> Option<&'static str> {
        None
    }

    /// Walks and running-app checks go through `ctx`, so they observe
    /// cancellation and record what could not be read.
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>>;

    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport>;

    /// Deny by default. True only when cleanup is exactly "move each listed
    /// path to Trash": the desktop then moves those paths itself and never
    /// calls `execute`. Everything else is report-only on the desktop.
    fn desktop_trash_paths(&self) -> bool {
        false
    }

    /// True when item paths come from a tool's output; such items must lie
    /// inside the home folder (see `trash_plan::protected_reason`).
    fn roots_from_tool_output(&self) -> bool {
        false
    }

    /// Why a provider without `desktop_trash_paths` is report-only.
    fn desktop_report_only_reason(&self) -> ReportOnly {
        ReportOnly::NotPerPathTrash
    }

    /// Apps that must not be running when `path` is acted on.
    fn item_apps(&self, _path: &Path) -> Vec<String> {
        self.quit_apps().into_iter().map(String::from).collect()
    }

    /// Provider-specific guard re-checked immediately before acting on
    /// `path`; `Err` carries the reason it is refused.
    fn check_item(&self, _path: &Path) -> std::result::Result<(), String> {
        Ok(())
    }
}

/// Why the desktop only reports a category instead of moving its items.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportOnly {
    /// Permanent deletion or Empty Trash.
    Destructive,
    /// Cleanup is a tool command, not a per-path move to Trash.
    NotPerPathTrash,
    /// The rule that selects items can flag data still in use.
    UnreliableMatch,
}

/// `None` when the desktop may move this provider's items to Trash.
pub fn desktop_report_only(provider: &dyn CleanProvider) -> Option<ReportOnly> {
    if provider.risk() == RiskLevel::Destructive {
        Some(ReportOnly::Destructive)
    } else if provider.desktop_trash_paths() {
        None
    } else {
        Some(provider.desktop_report_only_reason())
    }
}

/// Returns every provider in canonical id order. Filtering by risk level,
/// `--category`, and runtime availability happens in `discover.rs`.
///
/// Takes `&CleanOpts` so providers needing tunables (idle_days, search
/// roots, ...) can read them at construction. M0 providers don't yet, but
/// the param exists so M1+ can land without re-threading callers.
pub fn all_providers(opts: &crate::options::CleanOptions) -> Vec<Box<dyn CleanProvider>> {
    all_providers_with(opts, Arc::new(RealRunner))
}

/// `all_providers` with the runner tool-backed providers use, so the app can
/// pass its own tool lookup and tests a mock.
pub fn all_providers_with(
    opts: &crate::options::CleanOptions,
    runner: Arc<dyn CommandRunner>,
) -> Vec<Box<dyn CleanProvider>> {
    vec![
        Box::new(user_logs::UserLogs),
        Box::new(xcode::XcodeDerivedData),
        Box::new(user_caches::UserCaches::with_runner(runner.clone())),
        Box::new(xcode::XcodeArchives),
        Box::new(xcode::XcodeDeviceSupport),
        Box::new(dev_caches::CargoCache),
        Box::new(dev_caches::NpmCache::with_runner(runner.clone())),
        Box::new(dev_caches::PnpmStore::with_runner(runner.clone())),
        Box::new(dev_caches::YarnCache::with_runner(runner.clone())),
        Box::new(node_modules::NodeModules::with_runner(
            opts.idle_days,
            runner.clone(),
        )),
        Box::new(python_caches::PythonCaches::with_runner(
            opts.idle_days,
            runner.clone(),
        )),
        Box::new(rust_targets::RustTargets::with_runner(
            opts.idle_days,
            runner.clone(),
        )),
        Box::new(gradle_maven::GradleMaven),
        Box::new(jetbrains::JetBrains),
        Box::new(vscode::VsCode),
        Box::new(ios_simulators::IosSimulators),
        Box::new(simulator_devices::SimulatorDevices::with_runner(
            runner.clone(),
        )),
        Box::new(android_sdk::AndroidSdk::new(opts.idle_days)),
        Box::new(go_cache::GoCache::with_runner(runner.clone())),
        Box::new(docker::Docker::with_runner(runner.clone())),
        Box::new(docker::DockerVolumes::with_runner(runner.clone())),
        Box::new(downloads_old::DownloadsOld::with_runner(
            opts.idle_days,
            runner.clone(),
        )),
        Box::new(screenshots_old::ScreenshotsOld::with_runner(
            opts.idle_days,
            runner.clone(),
        )),
        Box::new(mail_attachments::MailAttachments),
        Box::new(streaming_caches::StreamingCaches::new()),
        Box::new(chat_caches::ChatCaches::new()),
        Box::new(browser_caches::BrowserCaches::new()),
        Box::new(quarantine::Quarantine),
        Box::new(crash_reports::CrashReports),
        Box::new(app_orphans::AppOrphans::with_runner(runner.clone())),
        Box::new(time_machine_local::TimeMachineLocal::with_runner(
            runner.clone(),
        )),
        Box::new(font_quicklook_caches::FontQuicklookCaches::with_runner(
            runner,
        )),
        Box::new(trash::TrashProvider),
    ]
}

/// Canonical list of category ids accepted by `--category`.
pub fn known_category_ids() -> &'static [&'static str] {
    &[
        "user-logs",
        "xcode-derived",
        "user-caches",
        "xcode-archives",
        "xcode-devicesupport",
        "cargo",
        "npm",
        "pnpm",
        "yarn",
        "node-modules",
        "python-caches",
        "rust-targets",
        "gradle-maven",
        "jetbrains",
        "vscode",
        "ios-simulators",
        "simulator-devices",
        "android-sdk",
        "go-cache",
        "docker",
        "docker-volumes",
        "downloads-old",
        "screenshots-old",
        "mail-attachments",
        "streaming-caches",
        "chat-caches",
        "browser-caches",
        "quarantine",
        "crash-reports",
        "app-orphans",
        "time-machine-local",
        "font-quicklook-caches",
        "trash",
    ]
}

/// Canonical set of dev project roots scanned by walking providers
/// (`node_modules`, `rust_targets`, `python_caches`). Only roots that exist
/// are returned. Order is stable so test discovery is deterministic.
pub(crate) fn dev_search_roots() -> Vec<std::path::PathBuf> {
    let h = match std::env::var_os("HOME") {
        Some(h) => std::path::PathBuf::from(h),
        None => return Vec::new(),
    };
    const ROOTS: &[&str] = &["Documents", "Projects", "Code", "Developer", "Workspace"];
    ROOTS
        .iter()
        .map(|r| h.join(r))
        .filter(|p| p.is_dir())
        .collect()
}

/// True iff `manifest_path` exists and was modified more than `idle_days`
/// days ago. Treats unreadable mtime as "not idle" (conservative — won't
/// flag a project we can't measure).
pub(crate) fn is_idle(manifest_path: &std::path::Path, idle_days: u64) -> bool {
    let meta = match std::fs::symlink_metadata(manifest_path) {
        Ok(m) => m,
        Err(_) => return false,
    };
    let mtime = match meta.modified() {
        Ok(t) => t,
        Err(_) => return false,
    };
    let elapsed = match std::time::SystemTime::now().duration_since(mtime) {
        Ok(d) => d,
        Err(_) => return false,
    };
    elapsed.as_secs() > idle_days * 86_400
}

/// Lists immediate children of `root` as `CleanItem`s, sized via the
/// symlink-safe walk. Returns empty when `root` does not exist.
pub(crate) fn top_level_entries(
    ctx: &ScanContext<'_>,
    root: &Path,
    category_id: &str,
    category_label: &str,
    risk: RiskLevel,
) -> Vec<CleanItem> {
    let mut out = Vec::new();
    for path in list_children(root, ctx) {
        let size = dir_size_checked(&path, ctx);
        out.push(CleanItem {
            category_id: category_id.to_string(),
            category_label: category_label.to_string(),
            path,
            size,
            risk,
            evidence: Vec::new(),
        });
    }
    out
}

/// Treats `root` itself as a single CleanItem (used for category-rooted
/// providers like xcode-derived). Returns empty when missing.
pub(crate) fn root_as_item(
    ctx: &ScanContext<'_>,
    root: &Path,
    category_id: &str,
    category_label: &str,
    risk: RiskLevel,
) -> Vec<CleanItem> {
    if !root.exists() {
        return Vec::new();
    }
    ctx.add_root(root);
    let size = dir_size_checked(root, ctx);
    vec![CleanItem {
        category_id: category_id.to_string(),
        category_label: category_label.to_string(),
        path: root.to_path_buf(),
        size,
        risk,
        evidence: Vec::new(),
    }]
}

/// Runs a discovery tool through the bounded runner. A missing tool, spawn
/// failure or timeout is an error (checked discovery reports the category
/// as failed); the exit status is left to the provider.
pub(crate) fn run_tool(
    runner: &dyn CommandRunner,
    bin: &str,
    args: &[&str],
) -> Result<CommandOutcome> {
    runner
        .output(bin, args, TOOL_TIMEOUT)
        .map_err(|e| engine_error!("{e}"))
}

/// Default per-item executor used by every provider except Trash. Rejects
/// `ExecAction::EmptyTrash`.
pub(crate) fn execute_per_item(
    items: &[CleanItem],
    action: ExecAction,
    provider_id: &'static str,
) -> Result<ExecReport> {
    if matches!(action, ExecAction::EmptyTrash) {
        return Err(engine_error!(
            "{} provider does not accept EmptyTrash",
            provider_id
        ));
    }
    let mut report = ExecReport::default();
    for item in items {
        let result = match action {
            ExecAction::Trash => move_to_trash(&item.path),
            ExecAction::HardDelete => remove_recursive_safe(&item.path)
                .map_err(|e| engine_error!("remove {}: {}", item.path.display(), e)),
            ExecAction::EmptyTrash => unreachable!(),
        };
        match result {
            Ok(()) => report.removed_paths.push(item.path.clone()),
            Err(e) => report.failed.push((item.path.clone(), e.to_string())),
        }
    }
    Ok(report)
}

/// Move a path to the user's Trash via Finder. Mirrors the helper in
/// `uninstall.rs` but lives here so the clean module is self-contained.
pub(crate) fn move_to_trash(path: &Path) -> Result<()> {
    let posix = path
        .to_str()
        .ok_or_else(|| engine_error!("non-utf8 path: {}", path.display()))?;
    let output = Command::new("osascript")
        .arg("-e")
        .arg(super::finder_trash::FINDER_DELETE_SCRIPT)
        .arg(posix)
        .output()
        .with_context(|| format!("failed to spawn osascript for {}", path.display()))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(engine_error!(
            "osascript failed for {}: {}",
            path.display(),
            err
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_known_category_has_family() {
        for id in known_category_ids() {
            // Must not panic. Result discarded; coverage is what we test.
            let _ = category_family(id);
        }
    }

    #[test]
    fn family_id_and_label_stable() {
        assert_eq!(Family::Dev.id(), "dev");
        assert_eq!(Family::UserStorage.id(), "user-storage");
        assert_eq!(Family::System.id(), "system");
        assert_eq!(Family::Dev.label(), "Dev caches");
        assert_eq!(Family::UserStorage.label(), "User storage");
        assert_eq!(Family::System.label(), "System leftovers");
    }

    /// Deliberate desktop decision for every category. A new provider fails
    /// this test until it is added here, so eligibility is never implicit.
    const DESKTOP_TRASH: &[(&str, Option<ReportOnly>)] = &[
        ("user-logs", None),
        ("xcode-derived", None),
        ("user-caches", None),
        ("xcode-archives", None),
        ("xcode-devicesupport", None),
        ("cargo", None),
        ("npm", None),
        ("pnpm", None),
        ("yarn", None),
        ("node-modules", None),
        ("python-caches", None),
        ("rust-targets", None),
        ("gradle-maven", None),
        ("jetbrains", None),
        ("vscode", None),
        ("ios-simulators", None),
        // Destructive: `xcrun simctl delete` removes the device for good.
        ("simulator-devices", Some(ReportOnly::Destructive)),
        ("android-sdk", None),
        ("go-cache", None),
        // Runs `docker image prune` / `docker builder prune`, not a per-path move.
        ("docker", Some(ReportOnly::NotPerPathTrash)),
        // Destructive: `docker volume prune` deletes container data.
        ("docker-volumes", Some(ReportOnly::Destructive)),
        ("downloads-old", None),
        ("screenshots-old", None),
        ("mail-attachments", None),
        ("streaming-caches", None),
        ("chat-caches", None),
        ("browser-caches", None),
        ("quarantine", None),
        ("crash-reports", None),
        // Folder names are matched against bundle IDs, so live data such as
        // `Code` (VS Code) or `AddressBook` is flagged as orphaned.
        ("app-orphans", Some(ReportOnly::UnreliableMatch)),
        // Destructive: `tmutil deletelocalsnapshots` and Empty Trash.
        ("time-machine-local", Some(ReportOnly::Destructive)),
        ("font-quicklook-caches", None),
        ("trash", Some(ReportOnly::Destructive)),
    ];

    #[test]
    fn every_category_has_a_deliberate_desktop_decision() {
        let decided: Vec<&str> = DESKTOP_TRASH.iter().map(|(id, _)| *id).collect();
        assert_eq!(decided, known_category_ids());
        let runner: Arc<dyn CommandRunner> =
            Arc::new(crate::runner::test_support::MockRunner::new());
        let providers = all_providers_with(&crate::options::CleanOptions::default(), runner);
        assert_eq!(providers.len(), DESKTOP_TRASH.len());
        for provider in providers {
            let expected = DESKTOP_TRASH
                .iter()
                .find(|(id, _)| *id == provider.id())
                .map(|(_, value)| *value);
            assert_eq!(
                Some(desktop_report_only(provider.as_ref())),
                expected,
                "{}",
                provider.id()
            );
        }
    }

    #[test]
    fn every_category_states_why_its_items_are_candidates() {
        let runner: Arc<dyn CommandRunner> =
            Arc::new(crate::runner::test_support::MockRunner::new());
        let opts = crate::options::CleanOptions {
            idle_days: 45,
            ..Default::default()
        };
        let providers = all_providers_with(&opts, runner);
        let ids: Vec<&str> = providers.iter().map(|p| p.id()).collect();
        assert_eq!(ids, known_category_ids());
        for provider in &providers {
            let reason = provider.inclusion_reason();
            assert!(!reason.trim().is_empty(), "{} has no reason", provider.id());
        }
        let reason_of = |id: &str| {
            providers
                .iter()
                .find(|p| p.id() == id)
                .unwrap()
                .inclusion_reason()
        };
        for idle in [
            "node-modules",
            "rust-targets",
            "python-caches",
            "downloads-old",
        ] {
            assert!(
                reason_of(idle).contains("45 days"),
                "{idle} names its threshold"
            );
        }
    }

    #[test]
    fn per_path_app_gates_name_the_owning_app() {
        let h = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
        let browser = browser_caches::BrowserCaches::new();
        let chrome = h.join("Library/Application Support/Google/Chrome/Default/Cache");
        assert_eq!(browser.item_apps(&chrome), vec!["Google Chrome"]);
        assert!(browser.item_apps(Path::new("/elsewhere")).len() > 1);
        let caches = user_caches::UserCaches::with_runner(Arc::new(
            crate::runner::test_support::MockRunner::new(),
        ));
        assert_eq!(
            caches.item_apps(&h.join("Library/Caches/com.apple.Safari")),
            vec!["Safari"]
        );
        assert_eq!(
            xcode::XcodeDerivedData.item_apps(Path::new("/x")),
            vec!["Xcode"]
        );
    }

    #[test]
    fn cargo_guard_refuses_everything_but_the_pinned_cache_dirs() {
        // Exercises the guard only; no execute or Trash path runs.
        let h = std::path::PathBuf::from(std::env::var_os("HOME").unwrap());
        let cargo = dev_caches::CargoCache;
        for pinned in ["registry/cache", "registry/src", "git/db", "git/checkouts"] {
            assert!(cargo.check_item(&h.join(".cargo").join(pinned)).is_ok());
        }
        for other in [".cargo/bin", ".cargo/credentials.toml", ".cargo", ".rustup"] {
            assert!(cargo.check_item(&h.join(other)).is_err(), "{other}");
        }
    }

    #[test]
    fn every_category_says_how_its_items_come_back() {
        for id in known_category_ids() {
            assert!(comes_back(id).is_some(), "{id}");
        }
        assert_eq!(comes_back("not-a-category"), None);
    }

    #[test]
    #[should_panic(expected = "unknown category id")]
    fn category_family_panics_on_unknown() {
        let _ = category_family("definitely-not-a-category");
    }
}
