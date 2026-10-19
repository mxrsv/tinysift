//! Records and enums the uninstall API exposes to Swift.

use tiny_core::uninstall::{ItemKind, LeftoverClass, Ownership, StopReason, UninstallRefusal};

use crate::clean::{FfiItemOutcome, FfiProtectionGuard};

/// One installed app. Selection uses `app_id`; `path` is for display only and
/// is never accepted back.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiInstalledApp {
    /// Opaque; valid only until the next `uninstall_list_apps`.
    pub app_id: String,
    pub name: String,
    pub bundle_id: Option<String>,
    pub path: String,
    pub size_bytes: u64,
    pub last_used_days: Option<u64>,
    /// Why the app cannot be uninstalled (a system app, a Homebrew cask);
    /// English diagnostic text. `None` when it can be previewed.
    pub blocked_reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiUninstallItemKind {
    AppBundle,
    Leftover,
}

/// What an item is, and whether anything but an explicit choice may remove it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum FfiUninstallOwnership {
    /// The `.app` bundle.
    App,
    /// An exact-name leftover the app rebuilds.
    Cache,
    /// An exact-name leftover holding settings or data; needs explicit selection.
    Data,
    /// Listed for information, never selectable.
    ReportOnly { reason: String },
    /// Could not be verified safe, never selectable.
    Uncertain { reason: String },
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiUninstallItem {
    /// Opaque; valid only for the preview that returned it.
    pub item_id: String,
    pub path: String,
    pub size_bytes: u64,
    pub kind: FfiUninstallItemKind,
    pub ownership: FfiUninstallOwnership,
    /// False for report-only and uncertain items and for held ones.
    pub selectable: bool,
    /// Why a saved "always keep" rule holds this path: a rule, Tiny's own
    /// storage, or a doubt whether a rule applies. A held item is shown but
    /// not selectable.
    pub held_by: Option<FfiProtectionGuard>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum FfiAppRunState {
    Running,
    NotRunning,
    /// The process name is unknown or the check failed; execution refuses.
    Unknown {
        detail: String,
    },
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiUninstallPreview {
    pub preview_id: String,
    pub app_id: String,
    pub app_name: String,
    pub items: Vec<FfiUninstallItem>,
    /// The app's state when the preview was built; execution checks again.
    pub run_state: FfiAppRunState,
    /// The bundle identifier the preview used; `None` when it could not be read.
    pub bundle_id: Option<String>,
    /// False when no leftovers were looked up (O1); only the app is listed.
    pub leftovers_searched: bool,
    /// Why leftovers were not searched (missing or invalid bundle id).
    pub leftovers_skipped_reason: Option<String>,
    /// Sum of the selectable items as measured now. Not freed space.
    pub selectable_bytes: u64,
    pub expires_in_seconds: u64,
    /// Unix seconds.
    pub expires_at: i64,
}

/// A run refused after the preview was spent (the app is running or its
/// check failed, its process name is unknown, a second copy appeared, or
/// the plan is inconsistent): build a new preview. Selection mistakes are
/// refused earlier as `FfiError::InvalidInput` and spend nothing;
/// `NothingSelected` and `NotSelectable` remain only as core's own defence.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum FfiUninstallRefusal {
    Blocked {
        reason: String,
    },
    Unreadable {
        path: String,
        error: String,
    },
    InconsistentPlan {
        detail: String,
    },
    NothingSelected,
    NotSelectable {
        item_id: String,
        reason: String,
    },
    ExecutableUnknown,
    AppRunning {
        app: String,
    },
    AppCheckFailed {
        app: String,
        error: String,
    },
    /// Another installed app now has the same bundle identifier.
    DuplicateBundleId,
    /// The installed apps could not be listed, so a second copy cannot be ruled out.
    InventoryUnavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum FfiUninstallStop {
    Cancelled,
    AutomationDenied,
    ProtectionChanged,
    ProtectionUnavailable {
        detail: String,
    },
    /// The app started after the run began.
    AppRunning {
        app: String,
    },
    AppCheckFailed {
        detail: String,
    },
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiUninstallItemResult {
    pub item_id: String,
    pub path: String,
    pub kind: FfiUninstallItemKind,
    pub size_bytes: u64,
    pub outcome: FfiItemOutcome,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiUninstallReport {
    /// One entry per selected item, in execution order (bundle first).
    pub results: Vec<FfiUninstallItemResult>,
    pub bytes_selected: u64,
    /// Not freed space: Trash keeps the items until it is emptied.
    pub bytes_moved_to_trash: u64,
    pub moved_count: u64,
    pub failed_count: u64,
    pub skipped_count: u64,
    pub not_attempted_count: u64,
    pub stopped: Option<FfiUninstallStop>,
}

impl From<ItemKind> for FfiUninstallItemKind {
    fn from(kind: ItemKind) -> Self {
        match kind {
            ItemKind::AppBundle => Self::AppBundle,
            ItemKind::Leftover => Self::Leftover,
        }
    }
}

impl From<&Ownership> for FfiUninstallOwnership {
    fn from(ownership: &Ownership) -> Self {
        match ownership {
            Ownership::AppBundle => Self::App,
            Ownership::Owned {
                class: LeftoverClass::Regenerable,
            } => Self::Cache,
            Ownership::Owned {
                class: LeftoverClass::DataRisk,
            } => Self::Data,
            Ownership::ReportOnly { reason } => Self::ReportOnly {
                reason: reason.clone(),
            },
            Ownership::Uncertain { reason } => Self::Uncertain {
                reason: reason.clone(),
            },
        }
    }
}

impl From<StopReason> for FfiUninstallStop {
    fn from(stop: StopReason) -> Self {
        match stop {
            StopReason::Cancelled => Self::Cancelled,
            StopReason::AutomationDenied => Self::AutomationDenied,
            StopReason::ProtectionChanged => Self::ProtectionChanged,
            StopReason::ProtectionUnavailable(detail) => Self::ProtectionUnavailable { detail },
            StopReason::AppRunning(app) => Self::AppRunning { app },
            StopReason::AppCheckFailed(detail) => Self::AppCheckFailed { detail },
        }
    }
}

/// Maps a core refusal; `item_id` names the plan index the way the caller saw it.
pub(super) fn refusal(
    refusal: UninstallRefusal,
    item_id: impl Fn(usize) -> String,
) -> FfiUninstallRefusal {
    match refusal {
        UninstallRefusal::Blocked(reason) => FfiUninstallRefusal::Blocked { reason },
        UninstallRefusal::Unreadable { path, error } => FfiUninstallRefusal::Unreadable {
            path: path.to_string_lossy().into_owned(),
            error,
        },
        UninstallRefusal::InconsistentPlan(detail) => {
            FfiUninstallRefusal::InconsistentPlan { detail }
        }
        UninstallRefusal::NothingSelected => FfiUninstallRefusal::NothingSelected,
        UninstallRefusal::UnknownItem(index) => FfiUninstallRefusal::InconsistentPlan {
            detail: format!("item {} is not in the plan", item_id(index)),
        },
        UninstallRefusal::NotSelectable { index, reason } => FfiUninstallRefusal::NotSelectable {
            item_id: item_id(index),
            reason,
        },
        UninstallRefusal::ExecutableUnknown => FfiUninstallRefusal::ExecutableUnknown,
        UninstallRefusal::AppRunning(app) => FfiUninstallRefusal::AppRunning { app },
        UninstallRefusal::AppCheckFailed { name, error } => {
            FfiUninstallRefusal::AppCheckFailed { app: name, error }
        }
    }
}
