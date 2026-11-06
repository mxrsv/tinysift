//! What `clean_remove` reports over FFI: each item's outcome, how much of
//! it is gone with the bytes measured, why any of it stayed, and the run's
//! totals.

use tiny_core::clean::checked_execute::{
    CheckedExecReport, ItemOutcome, RemovalExtent, StopReason,
};
use tiny_core::clean::failure_cause::FailureCause;
use tiny_core::clean::finder_trash::TrashError;
use tiny_core::clean::trash_plan::{DeleteMode, PlannedItem};

use super::super::records::skip_reason;
use super::super::{FfiSkipReason, FfiStopReason};

/// What a Clean action does with Rebuilt automatically and Project build
/// output, the app's Settings switch (OC1). Your files always go to the
/// Trash and a selected Trash row is always emptied.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiDeleteMode {
    /// Move them to the Trash, as `clean_preview` does.
    Trash,
    /// Delete them for good.
    Permanent,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiRemovalFailure {
    /// Finder Automation was denied for Empty Trash; moves go on.
    AutomationDenied,
    /// The item could not be moved to the Trash, or Finder could not empty
    /// it.
    TrashFailed,
    /// Permanent deletion failed part way or not at all.
    DeleteFailed,
    /// The category's own tool failed or could not run; part of a cache may
    /// be pruned.
    ToolFailed,
}

/// How one preview item ended. Nothing is ever retried another way: a failed
/// move is not deleted, a failed deletion is not moved or escalated.
#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum FfiRemovalOutcome {
    MovedToTrash,
    /// Deleted for good; its bytes are freed.
    Deleted,
    /// The Trash row: Finder emptied the Trash; its bytes are freed.
    TrashEmptied,
    /// The cache's own tool pruned it; `bytes_freed` is the cache's size
    /// before minus after.
    Pruned {
        bytes_freed: u64,
    },
    /// The category's tool accepted the removal and finishes it in the
    /// background (simulator runtimes): neither freed nor moved yet.
    RemovalRequested,
    Failed {
        reason: FfiRemovalFailure,
        detail: String,
        /// Re-checked after the failure. A failed deletion may still have
        /// removed part of the item.
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

/// How much of an item is gone, whatever its outcome says about the route.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiRemovalExtent {
    /// Deleted, pruned or emptied as planned.
    Removed,
    /// The removal failed part way (`outcome` is `Failed`): `bytes_freed`
    /// went, `bytes_left` stays.
    PartlyRemoved,
    /// Skipped, not attempted, or failed with nothing removed.
    NotRemoved,
    /// Moved to the Trash: still on disk, never freed.
    MovedToTrash,
    /// A tool accepted the removal and finishes it in the background.
    Requested,
}

/// Why an item, or part of it, stayed: one cause the app names with one
/// sentence and one action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiFailureCause {
    /// macOS refused TinySift: Full Disk Access, or a folder's own mode.
    NoPermission,
    /// An app is running, or the file is busy.
    InUse,
    /// Owned by another user or root.
    NeedsAdministrator,
    /// System Integrity Protection, a read-only volume, a system flag, or
    /// `/System`.
    ProtectedByMacos,
    /// Replaced, edited or added to since the scan.
    ChangedSinceScan,
    /// Finder Automation was denied (Empty Trash): System Settings ›
    /// Privacy & Security › Automation, not Full Disk Access.
    AutomationDenied,
    /// Gone before the run reached it.
    AlreadyGone,
    Other,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiRemovalResult {
    pub candidate_id: String,
    pub category_id: String,
    pub path: String,
    /// Discovery-time size: what was planned, not what was freed.
    pub size_bytes: u64,
    pub outcome: FfiRemovalOutcome,
    /// Always set by the core; `None` only in a record built elsewhere.
    #[uniffi(default = None)]
    pub extent: Option<FfiRemovalExtent>,
    /// Measured: the item's freed bytes right before its removal minus what
    /// is still there right after. 0 for moves, requests and items left as
    /// they were.
    #[uniffi(default = 0)]
    pub bytes_freed: u64,
    /// Measured right after a failed removal: what of the item stays.
    #[uniffi(default = 0)]
    pub bytes_left: u64,
    /// `bytes_freed` may be high (shared blocks the volume could not show,
    /// or a walk after the removal that could not read everything).
    #[uniffi(default = false)]
    pub bytes_freed_is_upper_bound: bool,
    /// Set for a skip, a failure and a partial removal (its first entry
    /// left); `None` when the item went as planned, was not attempted, or
    /// the user's saved rule kept it.
    #[uniffi(default = None)]
    pub cause: Option<FfiFailureCause>,
    /// English diagnostic text beside `cause`, for the run report. Never
    /// shown on screen.
    #[uniffi(default = None)]
    pub raw_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiRemovalReport {
    /// One entry per preview item, in preview order.
    pub results: Vec<FfiRemovalResult>,
    /// Discovery-time sizes of every preview item.
    pub bytes_selected: u64,
    /// Measured: the sum of every result's `bytes_freed` (deleted, pruned,
    /// emptied and partly removed items). Never a discovery-time size.
    pub bytes_freed: u64,
    /// `bytes_freed` counts blocks the volume could not show as unshared.
    pub bytes_freed_is_upper_bound: bool,
    /// Discovery-time sizes of the items moved to the Trash, which keeps
    /// them on disk: not freed space.
    pub bytes_moved_to_trash: u64,
    /// Items deleted or pruned, plus the Trash row when it was emptied.
    pub deleted_count: u64,
    pub moved_count: u64,
    pub failed_count: u64,
    pub skipped_count: u64,
    pub not_attempted_count: u64,
    pub stopped: Option<FfiStopReason>,
    /// Discovery-time sizes of the items whose removal a tool accepted and
    /// finishes in the background: neither freed nor moved.
    #[uniffi(default = 0)]
    pub bytes_requested: u64,
    /// The user cancelled the authentication dialog, which comes before any
    /// change: nothing changed, every item is `NotAttempted` and `stopped`
    /// is `Cancelled`.
    #[uniffi(default = false)]
    pub authentication_cancelled: bool,
    /// Items whose removal failed part way (`FfiRemovalExtent::PartlyRemoved`);
    /// they also count in `failed_count`.
    #[uniffi(default = 0)]
    pub partly_removed_count: u64,
    /// Measured: what the failed removals left, summed.
    #[uniffi(default = 0)]
    pub bytes_left: u64,
}

impl From<FfiDeleteMode> for DeleteMode {
    fn from(mode: FfiDeleteMode) -> Self {
        match mode {
            FfiDeleteMode::Trash => Self::Trash,
            FfiDeleteMode::Permanent => Self::Permanent,
        }
    }
}

pub(super) fn removal_report(
    items: &[(String, PlannedItem)],
    executed: CheckedExecReport,
) -> FfiRemovalReport {
    let count = |keep: fn(&ItemOutcome) -> bool| executed.count(keep) as u64;
    let summary = FfiRemovalReport {
        results: Vec::new(),
        bytes_selected: executed.bytes_selected(),
        bytes_freed: executed.bytes_freed,
        bytes_freed_is_upper_bound: executed.freed_is_upper_bound,
        bytes_moved_to_trash: executed.bytes_moved_to_trash(),
        deleted_count: count(ItemOutcome::frees_space),
        moved_count: executed.moved_count() as u64,
        failed_count: count(|o| {
            matches!(
                o,
                ItemOutcome::Failed { .. }
                    | ItemOutcome::DeleteFailed { .. }
                    | ItemOutcome::ToolFailed { .. }
            )
        }),
        skipped_count: count(|o| matches!(o, ItemOutcome::Skipped(_))),
        not_attempted_count: count(|o| matches!(o, ItemOutcome::NotAttempted)),
        stopped: executed.stopped.clone().map(Into::into),
        bytes_requested: executed.bytes_requested(),
        authentication_cancelled: executed.stopped == Some(StopReason::AuthenticationCancelled),
        partly_removed_count: executed.extent_count(RemovalExtent::PartlyRemoved) as u64,
        bytes_left: executed.bytes_left(),
    };
    let results = items
        .iter()
        .zip(executed.results)
        .map(|((id, _), result)| FfiRemovalResult {
            candidate_id: id.clone(),
            category_id: result.item.category_id,
            path: result.item.path.to_string_lossy().into_owned(),
            size_bytes: result.item.size,
            outcome: result.outcome.into(),
            extent: Some(result.extent.into()),
            bytes_freed: result.bytes_freed,
            bytes_left: result.bytes_left,
            bytes_freed_is_upper_bound: result.freed_is_upper_bound,
            cause: result.failure.as_ref().map(|f| f.cause.into()),
            raw_error: result.failure.map(|f| f.raw_error),
        })
        .collect();
    FfiRemovalReport { results, ..summary }
}

impl From<FailureCause> for FfiFailureCause {
    fn from(cause: FailureCause) -> Self {
        match cause {
            FailureCause::NoPermission => Self::NoPermission,
            FailureCause::InUse => Self::InUse,
            FailureCause::NeedsAdministrator => Self::NeedsAdministrator,
            FailureCause::ProtectedByMacOs => Self::ProtectedByMacos,
            FailureCause::ChangedSinceScan => Self::ChangedSinceScan,
            FailureCause::AutomationDenied => Self::AutomationDenied,
            FailureCause::AlreadyGone => Self::AlreadyGone,
            FailureCause::Other => Self::Other,
        }
    }
}

impl From<RemovalExtent> for FfiRemovalExtent {
    fn from(extent: RemovalExtent) -> Self {
        match extent {
            RemovalExtent::Removed => Self::Removed,
            RemovalExtent::PartlyRemoved => Self::PartlyRemoved,
            RemovalExtent::NotRemoved => Self::NotRemoved,
            RemovalExtent::MovedToTrash => Self::MovedToTrash,
            RemovalExtent::Requested => Self::Requested,
        }
    }
}

impl From<ItemOutcome> for FfiRemovalOutcome {
    fn from(outcome: ItemOutcome) -> Self {
        match outcome {
            ItemOutcome::MovedToTrash => Self::MovedToTrash,
            ItemOutcome::Deleted => Self::Deleted,
            ItemOutcome::TrashEmptied => Self::TrashEmptied,
            ItemOutcome::Failed {
                error,
                source_present,
            } => {
                let (reason, detail) = match error {
                    TrashError::AutomationDenied(d) => (FfiRemovalFailure::AutomationDenied, d),
                    TrashError::Failed(d) | TrashError::Os { detail: d, .. } => {
                        (FfiRemovalFailure::TrashFailed, d)
                    }
                };
                Self::Failed {
                    reason,
                    detail,
                    source_still_present: source_present,
                }
            }
            ItemOutcome::DeleteFailed {
                detail,
                source_present,
            } => Self::Failed {
                reason: FfiRemovalFailure::DeleteFailed,
                detail,
                source_still_present: source_present,
            },
            ItemOutcome::Skipped(reason) => {
                let (reason, detail) = skip_reason(reason);
                Self::Skipped { reason, detail }
            }
            ItemOutcome::NotAttempted => Self::NotAttempted,
            ItemOutcome::Pruned { bytes_freed, .. } => Self::Pruned { bytes_freed },
            ItemOutcome::RemovalRequested => Self::RemovalRequested,
            // The cache or runtime is still there, at most partly pruned.
            ItemOutcome::ToolFailed { detail } => Self::Failed {
                reason: FfiRemovalFailure::ToolFailed,
                detail,
                source_still_present: true,
            },
        }
    }
}
