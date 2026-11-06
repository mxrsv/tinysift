//! What a checked run reports: per item how it ended and what was measured
//! as freed, and the run's totals.

use std::path::PathBuf;

use crate::clean::failure_cause::Failure;
use crate::clean::finder_trash::TrashError;
use crate::clean::types::CleanItem;

/// Why a path was not moved even though it was planned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// The category is report-only on the desktop.
    ReportOnly,
    /// No registered provider has the item's category ID.
    UnknownCategory,
    Missing,
    /// The path itself is a symlink now.
    Symlink,
    /// Replaced or edited since discovery.
    Changed,
    /// Not inside a discovery root, or a symlink appeared on the way to it.
    OutsideRoots,
    /// The file-system root, the home folder or one of its ancestors, a
    /// tool-reported path outside the home folder, or anything in `/System`.
    ProtectedPath(String),
    /// On another volume than the home folder. Finder may delete such a
    /// path permanently when that volume has no Trash.
    NotOnHomeVolume,
    /// The provider's own guard refused the path.
    ProviderGuard(String),
    AppRunning(String),
    /// A required safety check could not run; acting would be a guess.
    SafetyCheckFailed(String),
    /// A saved rule keeps the path itself, or it lies inside a kept folder.
    Kept {
        rule_path: PathBuf,
    },
    /// The path is a parent of a saved rule's path, so moving it would carry
    /// the kept path along, found by discovery or not.
    HoldsKept {
        rule_path: PathBuf,
    },
    /// Whether a saved rule covers the path could not be decided.
    KeepUncertain(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemOutcome {
    MovedToTrash,
    /// Deleted for good (`Removal::Delete`), or gone after deletion as
    /// administrator.
    Deleted,
    /// Finder emptied the Trash (`Removal::EmptyTrash`).
    TrashEmptied,
    /// Finder refused to move the path or to empty the Trash.
    /// `source_present` is re-checked afterwards.
    Failed {
        error: TrashError,
        source_present: bool,
    },
    /// Permanent deletion failed, or the path is still there after deletion
    /// as administrator. Deletion goes on past entries that fail, so part of
    /// it may be gone (`RemovalExtent::PartlyRemoved`); `detail` names the
    /// first entry left. Nothing else is tried: no Trash, no other route,
    /// never administrator rights after a failure.
    DeleteFailed {
        detail: String,
        source_present: bool,
    },
    /// The tool pruned the cache folder: `bytes_freed` is its size before
    /// minus after, an upper bound when the volume could not tell which
    /// blocks other files share or the walk after could not read everything.
    Pruned {
        bytes_freed: u64,
        upper_bound: bool,
    },
    /// The tool accepted the removal and finishes it in the background
    /// (`simctl runtime delete`): neither freed nor moved yet.
    RemovalRequested,
    /// The tool command failed or could not run; part of the cache may be
    /// pruned. Nothing else is tried: no deletion, no Trash, no elevation.
    ToolFailed {
        detail: String,
    },
    Skipped(SkipReason),
    /// Execution stopped before this path.
    NotAttempted,
}

impl ItemOutcome {
    /// Deleted, pruned, or the Trash emptied: the item's bytes are free now.
    pub fn frees_space(&self) -> bool {
        matches!(
            self,
            Self::Deleted | Self::TrashEmptied | Self::Pruned { .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    Cancelled,
    /// The user cancelled the authentication dialog for deletion as
    /// administrator, which comes before anything changes.
    AuthenticationCancelled,
    AutomationDenied,
    /// The saved rules differ from what this run started with.
    ProtectionChanged,
    /// The saved rules could not be read again.
    ProtectionUnavailable(String),
}

/// How much of an item is gone, whatever route it took.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemovalExtent {
    /// Deleted, pruned or emptied as planned.
    Removed,
    /// The removal failed part way: some of the item is gone
    /// (`ItemResult::bytes_freed`), the rest stays (`bytes_left`).
    PartlyRemoved,
    /// Skipped, not attempted, or failed with nothing removed.
    NotRemoved,
    /// Moved to the Trash: still on disk, never counted as freed.
    MovedToTrash,
    /// A tool accepted the removal and finishes it in the background.
    Requested,
}

#[derive(Debug, Clone)]
pub struct ItemResult {
    pub item: CleanItem,
    pub outcome: ItemOutcome,
    pub extent: RemovalExtent,
    /// Measured: the item's freed bytes (the `freed_size` rule) right
    /// before its removal minus what is still there right after. 0 for
    /// moves, requests and items left as they were.
    pub bytes_freed: u64,
    /// Measured right after a removal that failed: what of the item stays.
    /// 0 when the removal succeeded (a prune keeps what is still in use).
    pub bytes_left: u64,
    /// `bytes_freed` may be high: the volume could not show some blocks as
    /// unshared, or the walk after could not read everything.
    pub freed_is_upper_bound: bool,
    /// Why the item, or part of it, stayed: for a skip, a failure or a
    /// partial removal (its first entry left). `None` when it went as
    /// planned, was not attempted, or a saved rule of the user kept it.
    pub failure: Option<Failure>,
}

#[derive(Debug, Clone, Default)]
pub struct CheckedExecReport {
    /// One entry per planned item, in plan order.
    pub results: Vec<ItemResult>,
    pub stopped: Option<StopReason>,
    /// The sum of every item's measured `bytes_freed`, partly removed
    /// items included. Items are measured one after another, right around
    /// their own removal, so blocks two items share count once.
    pub bytes_freed: u64,
    /// `bytes_freed` counts blocks the volume could not show as unshared.
    pub freed_is_upper_bound: bool,
}

impl CheckedExecReport {
    /// Sizes measured at discovery of every planned item.
    pub fn bytes_selected(&self) -> u64 {
        self.sum(|_| true)
    }

    /// Sizes measured at discovery of the items moved to Trash. Moving to
    /// Trash frees no space until Trash is emptied.
    pub fn bytes_moved_to_trash(&self) -> u64 {
        self.sum(|outcome| *outcome == ItemOutcome::MovedToTrash)
    }

    /// Sizes measured at discovery of the items whose removal a tool
    /// accepted and finishes in the background: neither freed nor moved.
    pub fn bytes_requested(&self) -> u64 {
        self.sum(|outcome| *outcome == ItemOutcome::RemovalRequested)
    }

    pub fn moved_count(&self) -> usize {
        self.count(|outcome| *outcome == ItemOutcome::MovedToTrash)
    }

    pub fn count(&self, keep: impl Fn(&ItemOutcome) -> bool) -> usize {
        self.results.iter().filter(|r| keep(&r.outcome)).count()
    }

    pub fn extent_count(&self, extent: RemovalExtent) -> usize {
        self.results.iter().filter(|r| r.extent == extent).count()
    }

    /// What the failed removals left, as measured right after each.
    pub fn bytes_left(&self) -> u64 {
        self.results
            .iter()
            .fold(0, |sum, r| sum.saturating_add(r.bytes_left))
    }

    fn sum(&self, keep: impl Fn(&ItemOutcome) -> bool) -> u64 {
        self.results
            .iter()
            .filter(|r| keep(&r.outcome))
            .fold(0, |sum, r| sum.saturating_add(r.item.size))
    }
}
