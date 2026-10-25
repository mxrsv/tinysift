use crate::engine_error;
use crate::error::Result;
use crate::runner::{CommandOutcome, CommandRunner, RealRunner, TOOL_TIMEOUT};
use std::path::Path;
use std::sync::Arc;

use super::fs_safe::{dir_size_checked, list_children, remove_recursive_safe};
use super::scan_context::ScanContext;
use super::trash_plan::inside_system;
use super::types::{CleanItem, ComesBack, ExecAction, ExecReport, RiskLevel};

pub use tool_command::DesktopTool;

pub mod android_sdk;
pub mod app_orphans;
pub mod browser_caches;
pub mod chat_caches;
pub mod codex_releases;
pub mod crash_reports;
pub mod dev_caches;
pub mod docker;
pub mod dot_cache;
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
pub mod simulator_runtime_images;
pub mod simulator_runtimes;
pub mod streaming_caches;
pub mod system_simulator_caches;
pub mod time_machine_local;
pub mod tool_command;
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
        "cargo" | "npm" | "pnpm" | "yarn" | "uv" => Family::Dev,
        "puppeteer" | "huggingface" | "codex-runtimes" | "codex-releases" => Family::Dev,
        "node-modules" | "python-caches" | "rust-targets" => Family::Dev,
        "gradle-maven" | "jetbrains" | "vscode" => Family::Dev,
        "ios-simulators" | "simulator-devices" | "android-sdk" => Family::Dev,
        "system-simulator-caches" | "simulator-runtimes" | "simulator-runtime-images" => {
            Family::Dev
        }
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
/// id outside `known_category_ids()`. It decides the desktop section
/// (`category_section`).
pub fn comes_back(category_id: &str) -> Option<ComesBack> {
    let rebuild = |command| Some(ComesBack::Rebuild { command });
    match category_id {
        "cargo" | "npm" | "pnpm" | "yarn" | "gradle-maven" | "android-sdk" | "docker" => {
            Some(ComesBack::Redownload)
        }
        "streaming-caches" | "chat-caches" => Some(ComesBack::Redownload),
        "uv" | "huggingface" | "codex-runtimes" | "codex-releases" => Some(ComesBack::Redownload),
        "simulator-runtimes" | "simulator-runtime-images" => Some(ComesBack::Redownload),
        "puppeteer" => rebuild("npx puppeteer browsers install"),
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
        "font-quicklook-caches" | "system-simulator-caches" => Some(ComesBack::AppRecreates),
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

/// The desktop section a category sits in, by what the user can expect
/// after cleanup. Report-only categories are grouped by their desktop
/// decision instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    /// A tool or app recreates the items.
    Rebuilt,
    /// Build output inside projects; a build or install recreates it.
    BuildOutput,
    /// Nothing recreates the items.
    YourFiles,
}

/// `None` for an id outside `known_category_ids()`. Project build output
/// has its own section; otherwise `comes_back` decides.
pub fn category_section(category_id: &str) -> Option<Section> {
    if matches!(category_id, "node-modules" | "rust-targets") {
        return Some(Section::BuildOutput);
    }
    comes_back(category_id).map(|back| match back {
        ComesBack::Rebuild { .. } | ComesBack::Redownload | ComesBack::AppRecreates => {
            Section::Rebuilt
        }
        ComesBack::TrashOnly | ComesBack::NotRecoverable => Section::YourFiles,
    })
}

pub trait CleanProvider {
    /// A provider-owned tool identifier, never an arbitrary relative filesystem path.
    fn owns_tool_id(&self, _path: &Path) -> bool {
        false
    }

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

    /// True only for `trash`: when the app asks for its row
    /// (`include_trash`), an explicitly selected row is emptied through
    /// Finder (`Removal::EmptyTrash`). `desktop_report_only` still says
    /// Destructive, so it is never moved or deleted path by path.
    fn desktop_empties_trash(&self) -> bool {
        false
    }

    /// The command the desktop may run instead of acting on paths, when the
    /// app asks for tool commands and the tool is found: `Prune` for a cache
    /// its tool prunes, `Request` for a removal its tool finishes in the
    /// background. `None` for every per-path category. `desktop_report_only`
    /// still names why a path-by-path action is not offered.
    fn desktop_tool(&self) -> Option<DesktopTool> {
        None
    }

    /// Runs the `desktop_tool` command for `item` through
    /// `tool_command::run_tool_command`; `Err` says why it failed.
    fn run_desktop_tool(&self, _item: &CleanItem) -> std::result::Result<(), String> {
        Err(format!("{} has no tool command", self.id()))
    }

    /// True when acting on the items needs administrator rights. The
    /// desktop never moves or deletes them unprivileged
    /// (`desktop_report_only`), only as administrator
    /// (`Removal::DeleteAsAdministrator`). The CLI, which never runs `sudo`,
    /// leaves such a category out unless it is named and refuses its items
    /// before `execute`.
    fn needs_admin(&self) -> bool {
        false
    }
}

/// Why the CLI refuses a category that `needs_admin`.
pub(crate) const NEEDS_ADMIN: &str = "needs administrator rights; the tiny CLI never runs sudo";

/// Why the desktop only reports a category instead of moving its items.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReportOnly {
    /// Permanent deletion or Empty Trash.
    Destructive,
    /// Cleanup is a tool command, not a per-path move to Trash.
    NotPerPathTrash,
    /// The rule that selects items can flag data still in use.
    UnreliableMatch,
    /// Acting needs administrator rights: no unprivileged route. The app
    /// deletes such items only as administrator.
    NeedsAdministrator,
    /// Xcode is not installed, so only System Settings > General > Storage
    /// can remove the items.
    XcodeNotInstalled,
}

/// `None` when the desktop may move this provider's items to Trash.
pub fn desktop_report_only(provider: &dyn CleanProvider) -> Option<ReportOnly> {
    if provider.risk() == RiskLevel::Destructive {
        Some(ReportOnly::Destructive)
    } else if provider.needs_admin() {
        Some(ReportOnly::NeedsAdministrator)
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
        Box::new(dev_caches::UvCache::with_runner(runner.clone())),
        Box::new(dot_cache::Puppeteer),
        Box::new(dot_cache::HuggingFace),
        Box::new(dot_cache::CodexRuntimes),
        Box::new(codex_releases::CodexReleases),
        Box::new(node_modules::NodeModules::with_options(
            opts,
            runner.clone(),
        )),
        Box::new(python_caches::PythonCaches::with_runner(
            opts.idle_days,
            runner.clone(),
        )),
        Box::new(rust_targets::RustTargets::with_options(
            opts,
            runner.clone(),
        )),
        Box::new(gradle_maven::GradleMaven),
        Box::new(jetbrains::JetBrains),
        Box::new(vscode::VsCode),
        Box::new(ios_simulators::IosSimulators),
        Box::new(simulator_devices::SimulatorDevices::with_runner(
            runner.clone(),
        )),
        Box::new(system_simulator_caches::SystemSimulatorCaches::with_runner(
            runner.clone(),
        )),
        Box::new(simulator_runtimes::SimulatorRuntimes::with_runner(
            runner.clone(),
        )),
        Box::new(simulator_runtime_images::SimulatorRuntimeImages::with_runner(runner.clone())),
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
        "uv",
        "puppeteer",
        "huggingface",
        "codex-runtimes",
        "codex-releases",
        "node-modules",
        "python-caches",
        "rust-targets",
        "gradle-maven",
        "jetbrains",
        "vscode",
        "ios-simulators",
        "simulator-devices",
        "system-simulator-caches",
        "simulator-runtimes",
        "simulator-runtime-images",
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

/// True for a directory named `node_modules`. Walks for other ecosystems stop
/// there: it holds most of a dev folder's entries and the node-modules
/// provider already lists it whole.
pub(crate) fn is_node_modules(dir: &std::path::Path) -> bool {
    dir.file_name().and_then(|n| n.to_str()) == Some("node_modules")
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
/// `ExecAction::EmptyTrash`, and refuses any path inside `/System`.
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
        if inside_system(&item.path) {
            report
                .failed
                .push((item.path.clone(), "inside /System".to_string()));
            continue;
        }
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

/// Runs a tool's own cleanup (`bin args...`) once for all `items`: the tool
/// decides what it removes, so every item shares the outcome. Unbounded, like
/// Docker's prune, because pruning a large cache can take minutes. Rejects
/// `EmptyTrash`; a missing tool removes nothing.
pub(crate) fn execute_tool_command(
    runner: &dyn CommandRunner,
    items: &[CleanItem],
    action: ExecAction,
    provider_id: &str,
    bin: &str,
    args: &[&str],
) -> Result<ExecReport> {
    if matches!(action, ExecAction::EmptyTrash) {
        return Err(engine_error!(
            "{} provider does not accept EmptyTrash",
            provider_id
        ));
    }
    let mut report = ExecReport::default();
    if items.is_empty() {
        return Ok(report);
    }
    let failure = if !runner.which(bin) {
        Some(format!("{bin} was not found; nothing was removed"))
    } else if runner.run(bin, args).success {
        None
    } else {
        Some(format!("{bin} {} failed", args.join(" ")))
    };
    for item in items {
        match &failure {
            None => report.removed_paths.push(item.path.clone()),
            Some(reason) => report.failed.push((item.path.clone(), reason.clone())),
        }
    }
    Ok(report)
}

/// Move a path to the user's Trash via Finder, never through a symlink.
pub(crate) fn move_to_trash(path: &Path) -> Result<()> {
    use super::finder_trash::{FinderTrash, Trash};
    FinderTrash
        .move_to_trash(path)
        .map_err(|e| engine_error!("{}: {e}", path.display()))
}

#[cfg(test)]
mod tests;
