//! Records and enums the cleanup API exposes to Swift, mirroring core data.

use tiny_core::clean::checked_execute::{ItemOutcome, SkipReason, StopReason};
use tiny_core::clean::finder_trash::TrashError;
use tiny_core::clean::providers::{ReportOnly, Section};
use tiny_core::clean::types::{ComesBack, Evidence, RiskLevel};
use tiny_core::options::CleanOptions;

use super::FfiProtectionGuard;

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiCleanOptions {
    /// Category IDs to scan; empty means "filter by risk flags".
    pub categories: Vec<String>,
    pub include_review: bool,
    pub include_destructive: bool,
    pub idle_days: u64,
    /// List every project's build output (`node-modules`, `rust-targets`),
    /// not only projects idle for `idle_days`: a candidate is Safe when its
    /// project is in a git work tree and has no edits, commits or uncommitted
    /// changes for 7 days, every fact checked, and Review otherwise (outside
    /// git too). The CLI never sets it.
    #[uniffi(default = false)]
    pub all_build_output: bool,
    /// Also scan the user's Trash (`trash`) and offer it as a row that an
    /// explicit selection empties for good (`FfiDesktopAction::EmptyTrash`).
    /// Other Destructive categories stay report only.
    #[uniffi(default = false)]
    pub include_trash: bool,
    /// Let the app act on categories cleaned by their own tool, when the
    /// tool is found: uv and pnpm caches become `MoveToTrash` (pruned by the
    /// tool with `FfiDeleteMode::Permanent`, moved to the Trash otherwise),
    /// simulator runtimes `RequestRemoval`. Off, they stay report only.
    #[uniffi(default = false)]
    pub tool_commands: bool,
}

/// One category a scan with the same options can return, known before the
/// scan. A category that does not apply on this Mac (its tool or folder is
/// absent) may still be left out of the scan's result.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiCleanPlace {
    pub id: String,
    pub label: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiRisk {
    Safe,
    Review,
    Destructive,
}

#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum FfiCategoryStatus {
    Found,
    /// The owning app is running, so the category was not scanned.
    AppRunning {
        app: String,
    },
    /// A required tool or safety probe is unavailable.
    Unavailable {
        reason: String,
    },
    Failed {
        detail: String,
    },
}

/// What the desktop may do with a category's candidates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiDesktopAction {
    /// The app may act on the candidates: a preview moves them to the Trash
    /// or, with `FfiDeleteMode::Permanent`, deletes Rebuilt automatically
    /// and Project build output for good, or has a cache's own tool prune it
    /// (uv, pnpm with `tool_commands`).
    MoveToTrash,
    /// The Trash row (`include_trash` only): a selected row is emptied for
    /// good through Finder, in either delete mode. Never preselected
    /// (Destructive).
    EmptyTrash,
    /// Simulator runtimes (`tool_commands` only): the category's own tool
    /// removes each selected item for good in either delete mode, and
    /// finishes in the background, so the result says it was requested.
    RequestRemoval,
    /// Root-owned system simulator caches (`needs_admin` is set): selected
    /// items are deleted for good as administrator, in either delete mode,
    /// after one macOS authentication dialog per Clean. No Trash route.
    DeleteAsAdministrator,
    /// Shown for information; the app does not act on it (`reason` says why).
    ReportOnly { reason: FfiReportOnlyReason },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiReportOnlyReason {
    /// Permanent deletion or Empty Trash.
    Destructive,
    /// Cleanup is a tool command (e.g. `docker image prune`), not a
    /// per-path move to Trash.
    NotPerPathTrash,
    /// The selection rule can flag data still in use (app-orphans).
    UnreliableMatch,
    /// Acting needs administrator rights. No longer sent: such categories
    /// are `FfiDesktopAction::DeleteAsAdministrator`.
    NeedsAdministrator,
    /// Xcode is not installed, so only System Settings > General > Storage
    /// removes these simulator runtimes (`simulator-runtime-images`).
    XcodeNotInstalled,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiUnreadablePath {
    pub path: String,
    pub entries: u64,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiCleanCandidate {
    /// Opaque; valid only for the discovery that returned it.
    pub id: String,
    pub path: String,
    pub size_bytes: u64,
    /// Entries the size walk could not read; non-zero makes `size_bytes` a
    /// lower bound.
    pub unreadable_entries: u64,
    pub risk: FfiRisk,
    /// Why this path was offered; never empty for a discovered candidate.
    pub evidence: Vec<FfiEvidence>,
    /// The volume could not tell which blocks other files share, so
    /// `size_bytes` counts them all: deleting frees at most that ("up to").
    #[uniffi(default = false)]
    pub size_is_upper_bound: bool,
}

/// One checkable fact behind a candidate; Swift writes the copy. Times are
/// Unix seconds.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum FfiEvidence {
    Modified {
        at: i64,
    },
    ManifestModified {
        file: String,
        at: i64,
    },
    LastCommit {
        at: i64,
    },
    WorkTreeClean,
    NotGitRepo,
    VenvMarker,
    LastOpened {
        at: i64,
    },
    OwningApp {
        name: String,
    },
    OwningAppUnknown,
    Sensitive {
        reason: String,
    },
    SimulatorUnavailable {
        name: String,
        reason: String,
    },
    /// A simulator runtime image: `runtimeIdentifier`, version and build.
    SimulatorRuntime {
        identifier: String,
        version: String,
        build: String,
    },
}

/// How a category's items come back after a move to Trash.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum FfiComesBack {
    Rebuild { command: String },
    Redownload,
    AppRecreates,
    TrashOnly,
    NotRecoverable,
}

/// The desktop section of a category's movable items, by what the user can
/// expect after cleanup. Report-only categories are grouped by
/// `desktop_action` instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiCleanSection {
    /// A tool or app recreates them ("Rebuilt automatically").
    Rebuilt,
    /// Build output inside projects: `rust-targets`, `node-modules`
    /// ("Project build output").
    BuildOutput,
    /// Nothing recreates them ("Your files").
    YourFiles,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiCleanCategory {
    pub id: String,
    pub label: String,
    /// Why this category's items are candidates; English diagnostic text
    /// Swift may show or replace with its own copy.
    pub inclusion_reason: String,
    /// `dev`, `user-storage` or `system`; `None` for an unregistered ID.
    pub family: Option<String>,
    pub risk: FfiRisk,
    pub status: FfiCategoryStatus,
    pub desktop_action: FfiDesktopAction,
    pub candidates: Vec<FfiCleanCandidate>,
    pub total_bytes: u64,
    /// Listing or search paths with unreadable entries.
    pub unreadable: Vec<FfiUnreadablePath>,
    /// Paths the provider found but that are never offered: the root, the
    /// home folder or its ancestors, tool output outside the home folder, or
    /// build output whose tracked files git could not list.
    pub refused: Vec<FfiRefusedPath>,
    /// `None` for an unregistered ID.
    pub comes_back: Option<FfiComesBack>,
    /// `None` for an unregistered ID.
    #[uniffi(default = None)]
    pub section: Option<FfiCleanSection>,
    /// Acting on the candidates needs administrator rights (a lock marker).
    #[uniffi(default = false)]
    pub needs_admin: bool,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiRefusedPath {
    pub path: String,
    /// English diagnostic text.
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiDiscovery {
    pub discovery_id: String,
    pub categories: Vec<FfiCleanCategory>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiPreviewItem {
    pub candidate_id: String,
    pub category_id: String,
    pub path: String,
    pub size_bytes: u64,
    pub risk: FfiRisk,
    /// Other discovered candidates at this path or inside it, selected or
    /// not, sorted by path. Moving this item moves them too.
    pub covers: Vec<FfiCoveredCandidate>,
    /// Removed for good: deleted, pruned by its tool, removal requested from
    /// its tool, or the Trash row emptied. False means moved to the Trash.
    #[uniffi(default = false)]
    pub permanent: bool,
    /// Deleted as administrator, behind the run's one authentication dialog
    /// (a lock marker). Always `permanent`.
    #[uniffi(default = false)]
    pub needs_admin: bool,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiCoveredCandidate {
    pub candidate_id: String,
    pub path: String,
    pub risk: FfiRisk,
    pub selected: bool,
}

#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum FfiExclusionReason {
    /// Another selected category found the same path.
    Duplicate { kept_candidate_id: String },
    /// A selected ancestor moves this path with it.
    InsideSelected { parent_candidate_id: String },
    /// Moving this path would also move Review-risk candidates the user did
    /// not select (PC-C3); it is kept only when all of them are selected.
    CoversUnselectedReview { candidate_ids: Vec<String> },
    /// A saved "always keep" rule holds this path (or Tiny's own storage, or
    /// whether a rule applies could not be decided). Not a Review risk: only
    /// removing the rule (or fixing the store) lets it move. Its selected
    /// siblings stay in the preview.
    Protected { held_by: FfiProtectionGuard },
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiPreviewExclusion {
    pub candidate_id: String,
    pub path: String,
    pub reason: FfiExclusionReason,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiPreview {
    pub preview_id: String,
    pub items: Vec<FfiPreviewItem>,
    pub excluded: Vec<FfiPreviewExclusion>,
    /// Sum of `items` sizes as measured at discovery. Not freed space.
    pub bytes_selected: u64,
    pub expires_in_seconds: u64,
    /// What the deleted items, prunes and the Trash row free together: their
    /// sizes, plus blocks they share only with each other (hard links, APFS
    /// clones) counted once. A prune frees at most its cache's size, so it
    /// makes this an upper bound. Report-only candidates are never in a
    /// preview.
    #[uniffi(default = 0)]
    pub bytes_to_free: u64,
    /// `bytes_to_free` counts blocks the volume could not show as unshared
    /// ("up to").
    #[uniffi(default = false)]
    pub bytes_to_free_is_upper_bound: bool,
    /// Sum of the sizes of the items moved to the Trash, which frees nothing
    /// until the Trash is emptied.
    #[uniffi(default = 0)]
    pub bytes_to_trash: u64,
    /// Sum of the sizes of the items whose removal a tool is asked for
    /// (simulator runtimes): neither freed nor moved when the run ends.
    #[uniffi(default = 0)]
    pub bytes_requested: u64,
    /// Sum of the sizes of the items deleted as administrator, part of
    /// `bytes_to_free`. Non-zero means the run shows the authentication
    /// dialog once, before anything changes.
    #[uniffi(default = 0)]
    pub bytes_needing_admin: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiTrashFailure {
    AutomationDenied,
    TrashFailed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiSkipReason {
    ReportOnly,
    UnknownCategory,
    Missing,
    Symlink,
    /// Replaced or edited since discovery.
    Changed,
    OutsideScanRoots,
    /// The root, the home folder or an ancestor, a tool-reported path
    /// outside the home folder, or anything in `/System`.
    ProtectedPath,
    /// On another volume than the home folder, where Finder may delete
    /// permanently instead of moving to Trash.
    NotOnHomeVolume,
    ProviderGuard,
    AppRunning,
    /// A required safety check could not run.
    SafetyCheckUnavailable,
    /// A saved "always keep" rule covers it. `detail` is the rule's path.
    Kept,
    /// It would carry a kept path along, found by the scan or not. `detail`
    /// is that path.
    HoldsKept,
    /// Whether a saved rule covers it could not be decided; `detail` says why.
    KeepUncertain,
}

#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum FfiItemOutcome {
    MovedToTrash,
    /// The source is re-checked after the failure; nothing else is tried.
    Failed {
        reason: FfiTrashFailure,
        detail: String,
        source_still_present: bool,
    },
    Skipped {
        reason: FfiSkipReason,
        /// The guard message, app name, failed check or kept path, when there
        /// is one.
        detail: Option<String>,
    },
    NotAttempted,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum FfiStopReason {
    Cancelled,
    AutomationDenied,
    /// The saved "always keep" rules changed while the run was moving
    /// items; the rest were not attempted.
    ProtectionChanged,
    /// The saved rules could not be read again mid-run; the rest were not
    /// attempted.
    ProtectionUnavailable {
        detail: String,
    },
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiItemResult {
    pub candidate_id: String,
    pub category_id: String,
    pub path: String,
    pub size_bytes: u64,
    pub outcome: FfiItemOutcome,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiExecReport {
    /// One entry per preview item, in preview order.
    pub results: Vec<FfiItemResult>,
    /// Discovery-time sizes of every preview item.
    pub bytes_selected: u64,
    /// Discovery-time sizes of the moved items. Trash keeps them on disk,
    /// so this is not freed space.
    pub bytes_moved_to_trash: u64,
    pub moved_count: u64,
    pub failed_count: u64,
    pub skipped_count: u64,
    pub not_attempted_count: u64,
    pub stopped: Option<FfiStopReason>,
}

impl From<FfiCleanOptions> for CleanOptions {
    fn from(options: FfiCleanOptions) -> Self {
        Self {
            category: options.categories,
            include_review: options.include_review,
            include_destructive: options.include_destructive,
            idle_days: options.idle_days,
            all_build_output: options.all_build_output,
            include_trash: options.include_trash,
            tool_commands: options.tool_commands,
        }
    }
}

impl From<Evidence> for FfiEvidence {
    fn from(evidence: Evidence) -> Self {
        match evidence {
            Evidence::Modified { at } => Self::Modified { at },
            Evidence::ManifestModified { file, at } => Self::ManifestModified { file, at },
            Evidence::LastCommit { at } => Self::LastCommit { at },
            Evidence::WorkTreeClean => Self::WorkTreeClean,
            Evidence::NotGitRepo => Self::NotGitRepo,
            Evidence::VenvMarker => Self::VenvMarker,
            Evidence::LastOpened { at } => Self::LastOpened { at },
            Evidence::OwningApp { name } => Self::OwningApp { name },
            Evidence::OwningAppUnknown => Self::OwningAppUnknown,
            Evidence::Sensitive { reason } => Self::Sensitive { reason },
            Evidence::SimulatorUnavailable { name, reason } => {
                Self::SimulatorUnavailable { name, reason }
            }
            Evidence::SimulatorRuntime {
                identifier,
                version,
                build,
            } => Self::SimulatorRuntime {
                identifier,
                version,
                build,
            },
        }
    }
}

impl From<ComesBack> for FfiComesBack {
    fn from(comes_back: ComesBack) -> Self {
        match comes_back {
            ComesBack::Rebuild { command } => Self::Rebuild {
                command: command.to_string(),
            },
            ComesBack::Redownload => Self::Redownload,
            ComesBack::AppRecreates => Self::AppRecreates,
            ComesBack::TrashOnly => Self::TrashOnly,
            ComesBack::NotRecoverable => Self::NotRecoverable,
        }
    }
}

impl From<Section> for FfiCleanSection {
    fn from(section: Section) -> Self {
        match section {
            Section::Rebuilt => Self::Rebuilt,
            Section::BuildOutput => Self::BuildOutput,
            Section::YourFiles => Self::YourFiles,
        }
    }
}

impl From<RiskLevel> for FfiRisk {
    fn from(risk: RiskLevel) -> Self {
        match risk {
            RiskLevel::Safe => Self::Safe,
            RiskLevel::Review => Self::Review,
            RiskLevel::Destructive => Self::Destructive,
        }
    }
}

impl From<ReportOnly> for FfiReportOnlyReason {
    fn from(reason: ReportOnly) -> Self {
        match reason {
            ReportOnly::Destructive => Self::Destructive,
            ReportOnly::NotPerPathTrash => Self::NotPerPathTrash,
            ReportOnly::UnreliableMatch => Self::UnreliableMatch,
            ReportOnly::NeedsAdministrator => Self::NeedsAdministrator,
            ReportOnly::XcodeNotInstalled => Self::XcodeNotInstalled,
        }
    }
}

impl From<StopReason> for FfiStopReason {
    fn from(reason: StopReason) -> Self {
        match reason {
            // `FfiRemovalReport.authentication_cancelled` tells them apart.
            StopReason::Cancelled | StopReason::AuthenticationCancelled => Self::Cancelled,
            StopReason::AutomationDenied => Self::AutomationDenied,
            StopReason::ProtectionChanged => Self::ProtectionChanged,
            StopReason::ProtectionUnavailable(detail) => Self::ProtectionUnavailable { detail },
        }
    }
}

impl From<ItemOutcome> for FfiItemOutcome {
    fn from(outcome: ItemOutcome) -> Self {
        match outcome {
            ItemOutcome::MovedToTrash => Self::MovedToTrash,
            ItemOutcome::Failed {
                error,
                source_present,
            } => {
                let (reason, detail) = match error {
                    TrashError::AutomationDenied(d) => (FfiTrashFailure::AutomationDenied, d),
                    TrashError::Failed(d) => (FfiTrashFailure::TrashFailed, d),
                };
                Self::Failed {
                    reason,
                    detail,
                    source_still_present: source_present,
                }
            }
            ItemOutcome::Skipped(reason) => {
                let (reason, detail) = skip_reason(reason);
                Self::Skipped { reason, detail }
            }
            ItemOutcome::NotAttempted => Self::NotAttempted,
            // `clean_execute` refuses a preview that removes anything for
            // good, and uninstall only moves to the Trash.
            ItemOutcome::Deleted
            | ItemOutcome::TrashEmptied
            | ItemOutcome::DeleteFailed { .. }
            | ItemOutcome::Pruned { .. }
            | ItemOutcome::RemovalRequested
            | ItemOutcome::ToolFailed { .. } => {
                unreachable!("a Trash-only run reported a permanent removal")
            }
        }
    }
}

pub(super) fn skip_reason(reason: SkipReason) -> (FfiSkipReason, Option<String>) {
    match reason {
        SkipReason::ReportOnly => (FfiSkipReason::ReportOnly, None),
        SkipReason::UnknownCategory => (FfiSkipReason::UnknownCategory, None),
        SkipReason::Missing => (FfiSkipReason::Missing, None),
        SkipReason::Symlink => (FfiSkipReason::Symlink, None),
        SkipReason::Changed => (FfiSkipReason::Changed, None),
        SkipReason::OutsideRoots => (FfiSkipReason::OutsideScanRoots, None),
        SkipReason::ProtectedPath(d) => (FfiSkipReason::ProtectedPath, Some(d)),
        SkipReason::NotOnHomeVolume => (FfiSkipReason::NotOnHomeVolume, None),
        SkipReason::ProviderGuard(d) => (FfiSkipReason::ProviderGuard, Some(d)),
        SkipReason::AppRunning(app) => (FfiSkipReason::AppRunning, Some(app)),
        SkipReason::SafetyCheckFailed(d) => (FfiSkipReason::SafetyCheckUnavailable, Some(d)),
        SkipReason::Kept { rule_path } => (FfiSkipReason::Kept, Some(lossy(&rule_path))),
        SkipReason::HoldsKept { rule_path } => (FfiSkipReason::HoldsKept, Some(lossy(&rule_path))),
        SkipReason::KeepUncertain(d) => (FfiSkipReason::KeepUncertain, Some(d)),
    }
}

fn lossy(path: &std::path::Path) -> String {
    path.to_string_lossy().into_owned()
}
