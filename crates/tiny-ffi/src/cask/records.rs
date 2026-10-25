//! Records and enums the cask API exposes to Swift.

use std::path::{Path, PathBuf};

use tiny_core::cask::{
    BrewEnvWarning, CaskAction, CaskFailure, CaskPlanItem, CaskProgress, CatalogueApp, Group,
    HomebrewStatus, OutdatedCask, RunOutcome, RunState, Stage,
};

use crate::uninstall::FfiAppRunState;

/// A setting in a `brew.env` file that `brew` applies over the environment
/// TinySift runs it with: greedy upgrades, `HOMEBREW_NO_SUDO` or
/// `HOMEBREW_NO_AUTO_UPDATE`. TinySift cannot stop it, so it says so.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiBrewEnvWarning {
    pub file: String,
    pub variable: String,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum FfiHomebrewStatus {
    /// Absolute path of the `brew` that would run, and the settings of its
    /// `brew.env` files that override ours.
    Present {
        path: String,
        warnings: Vec<FfiBrewEnvWarning>,
    },
    /// No Homebrew on this Mac. TinySift never installs it.
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiCaskGroup {
    Recommended,
    More,
}

/// One app of the bundled list.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiCaskCatalogueApp {
    /// The Homebrew cask token; the only name `cask_install_preview` accepts.
    pub token: String,
    pub name: String,
    pub blurb: String,
    pub group: FfiCaskGroup,
    pub bundle_id: String,
    /// An installed app has this bundle identifier. Matched by identity, not
    /// by name.
    pub on_this_mac: bool,
    /// Homebrew has a record of the cask, whether or not its app is still on
    /// the Mac. An install of it is refused (`AlreadyInstalled`). `false` when
    /// Homebrew could not be asked.
    pub tracked_by_homebrew: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiCaskAction {
    Install,
    Update,
}

/// Which problem stopped a request, with plain English text for it. The kind
/// is the contract; `problem` and `recovery` are diagnostics the client may
/// replace with its own copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiCaskFailureKind {
    HomebrewMissing,
    NotInCatalogue,
    NotOutdated,
    NeedsAdministrator,
    /// The cask only has an installer to open by hand.
    ManualInstaller,
    /// The app is open: quit it, then ask again.
    AppRunning,
    AppCheckFailed,
    AppExists,
    AlreadyInstalled,
    Network,
    /// The download does not match the checksum Homebrew's data holds: its
    /// data is probably old.
    StaleData,
    CaskNotFound,
    TimedOut,
    BrewFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiCaskFailure {
    pub kind: FfiCaskFailureKind,
    pub problem: String,
    pub recovery: String,
    /// What the failure is about: the app that is open or in the way (a
    /// path), or the cask token.
    pub subject: Option<String>,
    /// `brew`'s own words or the underlying error, shortened.
    pub detail: Option<String>,
}

/// What a run will do to one cask. `app_path` is for display and for Open;
/// it is never accepted back.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiCaskPreviewItem {
    pub token: String,
    pub name: String,
    /// The version on the Mac now; `None` for an install.
    pub from_version: Option<String>,
    pub version: String,
    pub download_host: Option<String>,
    /// The exact command, for example `brew install --cask ghostty`.
    pub command: String,
    /// Why the run would be refused (`NeedsAdministrator`, `ManualInstaller`),
    /// so the sheet disables its forward button and says why.
    pub refusal: Option<FfiCaskFailure>,
    pub app_path: Option<String>,
    /// For an update; an install is always `NotRunning`. A run checks again.
    pub run_state: FfiAppRunState,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiCaskPreview {
    pub preview_id: String,
    pub action: FfiCaskAction,
    pub items: Vec<FfiCaskPreviewItem>,
    /// `brew.env` settings that can undo how TinySift runs `brew`.
    pub warnings: Vec<FfiBrewEnvWarning>,
    pub expires_in_seconds: u64,
    /// Unix seconds.
    pub expires_at: i64,
}

/// An app Homebrew tracks that has a newer version.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiOutdatedCask {
    pub token: String,
    pub name: String,
    pub from_version: String,
    pub version: String,
    pub download_host: Option<String>,
    /// Why updating it would be refused (`NeedsAdministrator`).
    pub refusal: Option<FfiCaskFailure>,
    pub app_path: Option<String>,
    pub run_state: FfiAppRunState,
}

/// How far one cask is. Never a percentage: `brew` prints none.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiCaskStage {
    Downloading,
    Installing,
    /// Before any line `brew` prints is recognised.
    Working,
}

/// One step of a run, for the cask it names: an update shows one app at a
/// time.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiCaskProgress {
    pub token: String,
    pub stage: FfiCaskStage,
    /// Zero-based position of this cask among the `total` in the run.
    pub position: u64,
    pub total: u64,
}

/// Implemented in Swift. Called on the thread that runs `brew`; Swift hops to
/// the main actor before touching UI state.
#[uniffi::export(with_foreign)]
pub trait CaskProgressListener: Send + Sync {
    fn on_progress(&self, progress: FfiCaskProgress);
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum FfiCaskOutcome {
    Done,
    Cancelled,
    /// An earlier cask was cancelled, so this one never started.
    NotAttempted,
    Failed {
        failure: FfiCaskFailure,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiCaskItemResult {
    pub token: String,
    pub name: String,
    pub app_path: Option<String>,
    pub outcome: FfiCaskOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiCaskReport {
    pub action: FfiCaskAction,
    /// One per cask run, in the order they ran.
    pub results: Vec<FfiCaskItemResult>,
    pub done_count: u64,
    pub failed_count: u64,
    pub cancelled: bool,
}

impl From<&BrewEnvWarning> for FfiBrewEnvWarning {
    fn from(warning: &BrewEnvWarning) -> Self {
        Self {
            file: lossy(&warning.file),
            variable: warning.variable.clone(),
        }
    }
}

pub(super) fn homebrew_status(
    status: HomebrewStatus,
    warnings: &[BrewEnvWarning],
) -> FfiHomebrewStatus {
    match status {
        HomebrewStatus::Present(path) => FfiHomebrewStatus::Present {
            path: lossy(&path),
            warnings: warnings.iter().map(Into::into).collect(),
        },
        HomebrewStatus::Missing => FfiHomebrewStatus::Missing,
    }
}

impl From<Stage> for FfiCaskStage {
    fn from(stage: Stage) -> Self {
        match stage {
            Stage::Downloading => Self::Downloading,
            Stage::Installing => Self::Installing,
            Stage::Working => Self::Working,
        }
    }
}

impl From<CaskProgress> for FfiCaskProgress {
    fn from(progress: CaskProgress) -> Self {
        Self {
            token: progress.token,
            stage: progress.stage.into(),
            position: progress.position,
            total: progress.total,
        }
    }
}

impl From<RunState> for FfiAppRunState {
    fn from(state: RunState) -> Self {
        match state {
            RunState::NotRunning => Self::NotRunning,
            RunState::Running { .. } => Self::Running,
            RunState::Unknown { error, .. } => Self::Unknown { detail: error },
        }
    }
}

impl From<Group> for FfiCaskGroup {
    fn from(group: Group) -> Self {
        match group {
            Group::Recommended => Self::Recommended,
            Group::More => Self::More,
        }
    }
}

pub(super) fn catalogue_record(app: CatalogueApp, tracked: bool) -> FfiCaskCatalogueApp {
    let entry = app.entry;
    FfiCaskCatalogueApp {
        token: entry.token.into(),
        name: entry.name.into(),
        blurb: entry.blurb.into(),
        group: entry.group.into(),
        bundle_id: entry.bundle_id.into(),
        on_this_mac: app.on_this_mac,
        tracked_by_homebrew: tracked,
    }
}

impl From<CaskAction> for FfiCaskAction {
    fn from(action: CaskAction) -> Self {
        match action {
            CaskAction::Install => Self::Install,
            CaskAction::Update => Self::Update,
        }
    }
}

impl From<&CaskFailure> for FfiCaskFailure {
    fn from(failure: &CaskFailure) -> Self {
        use FfiCaskFailureKind as Kind;
        let (kind, subject) = match failure {
            CaskFailure::HomebrewMissing => (Kind::HomebrewMissing, None),
            CaskFailure::NotInCatalogue { token } => (Kind::NotInCatalogue, Some(token)),
            CaskFailure::NotOutdated { token } => (Kind::NotOutdated, Some(token)),
            CaskFailure::NeedsAdministrator => (Kind::NeedsAdministrator, None),
            CaskFailure::ManualInstaller { token } => (Kind::ManualInstaller, Some(token)),
            CaskFailure::AppRunning { app } => (Kind::AppRunning, Some(app)),
            CaskFailure::AppCheckFailed { app, .. } => (Kind::AppCheckFailed, Some(app)),
            CaskFailure::AppExists { path } => (Kind::AppExists, Some(path)),
            CaskFailure::AlreadyInstalled { token } => (Kind::AlreadyInstalled, Some(token)),
            CaskFailure::Network { .. } => (Kind::Network, None),
            CaskFailure::StaleData => (Kind::StaleData, None),
            CaskFailure::CaskNotFound { token } => (Kind::CaskNotFound, Some(token)),
            CaskFailure::TimedOut => (Kind::TimedOut, None),
            CaskFailure::BrewFailed { .. } => (Kind::BrewFailed, None),
        };
        Self {
            kind,
            problem: failure.problem(),
            recovery: failure.recovery(),
            subject: subject.cloned(),
            detail: failure.detail().map(String::from),
        }
    }
}

impl From<&RunOutcome> for FfiCaskOutcome {
    fn from(outcome: &RunOutcome) -> Self {
        match outcome {
            RunOutcome::Done => Self::Done,
            RunOutcome::Cancelled => Self::Cancelled,
            RunOutcome::NotAttempted => Self::NotAttempted,
            RunOutcome::Failed(failure) => Self::Failed {
                failure: failure.into(),
            },
        }
    }
}

pub(super) fn preview_item(item: &CaskPlanItem, run_state: FfiAppRunState) -> FfiCaskPreviewItem {
    FfiCaskPreviewItem {
        token: item.token.clone(),
        name: item.name.clone(),
        from_version: item.from_version.clone(),
        version: item.version.clone(),
        download_host: item.download_host.clone(),
        command: item.command(),
        refusal: item.blocked.as_ref().map(Into::into),
        app_path: first_app(&item.apps),
        run_state,
    }
}

pub(super) fn outdated_record(cask: &OutdatedCask, run_state: FfiAppRunState) -> FfiOutdatedCask {
    FfiOutdatedCask {
        token: cask.token.clone(),
        name: cask.name.clone(),
        from_version: cask.from_version.clone(),
        version: cask.version.clone(),
        download_host: cask.download_host.clone(),
        refusal: cask.blocked.as_ref().map(Into::into),
        app_path: first_app(&cask.apps),
        run_state,
    }
}

pub(super) fn first_app(apps: &[PathBuf]) -> Option<String> {
    apps.first().map(|path| lossy(path))
}

fn lossy(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}
