//! Records Swift sees for Large Files. Paths cross only outward: Swift names
//! file IDs, preview IDs and move IDs, never a path to act on.

use tiny_core::clean::failure_cause::Failure;
use tiny_core::clean::large_files::undo::UndoOutcome;
use tiny_core::clean::large_files::LargeFile;

use crate::clean::{FfiFailureCause, FfiItemOutcome, FfiStopReason};

/// One row of the list.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiLargeFile {
    /// Valid until the next scan.
    pub id: String,
    pub path: String,
    /// How a person names it ("Cap installer").
    pub name: String,
    /// The group's stable identifier ("installers").
    pub group: String,
    /// One sentence from a fact the scan checked.
    pub reason: String,
    /// What moving it frees once the Trash is emptied: private bytes, so a
    /// file whose blocks are shared frees less than its size.
    pub frees_bytes: u64,
    /// False when the volume could not vouch for every block: `frees_bytes`
    /// is then an upper bound ("at most").
    pub frees_is_exact: bool,
    /// The app bundle the file was matched to, for its icon.
    pub matched_app_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiLargeFilesScan {
    pub scan_id: String,
    /// By what each frees, largest first.
    pub files: Vec<FfiLargeFile>,
    /// Folders that exist but could not be listed: nothing found there does
    /// not mean nothing is there.
    pub unreadable_folders: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiLargeFilesPreview {
    pub preview_id: String,
    /// What the move will take, in the order asked.
    pub items: Vec<FfiLargeFile>,
    /// Asked for but held by a saved "always keep" rule: not in `items`.
    pub held_ids: Vec<String>,
    /// Sum of `items`' `frees_bytes`.
    pub frees_bytes: u64,
    pub frees_is_exact: bool,
    pub expires_in_seconds: u64,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiLargeFileMoveResult {
    pub file_id: String,
    pub path: String,
    pub outcome: FfiItemOutcome,
    /// Set for a skip and a failure; `None` when it moved, was not attempted,
    /// or a saved rule of the user kept it.
    pub cause: Option<FfiFailureCause>,
    /// English diagnostic text beside `cause`, for the run report only.
    pub raw_error: Option<String>,
    /// Measured right before a move: what emptying the Trash frees. A move
    /// frees nothing by itself.
    pub bytes_moved: u64,
    pub bytes_moved_is_upper_bound: bool,
    /// Where the Trash put it, when macOS said.
    pub trashed_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiLargeFilesMoveReport {
    /// What Undo takes; `None` when nothing could be put back (nothing moved,
    /// or the Trash did not say where anything went).
    pub move_id: Option<String>,
    /// One per preview item, in preview order.
    pub results: Vec<FfiLargeFileMoveResult>,
    /// The sum of `bytes_moved`: freed once the Trash is emptied, not now.
    pub bytes_moved_to_trash: u64,
    pub bytes_moved_is_upper_bound: bool,
    pub moved_count: u64,
    pub failed_count: u64,
    pub skipped_count: u64,
    pub not_attempted_count: u64,
    pub stopped: Option<FfiStopReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum FfiUndoOutcome {
    Restored,
    /// Something exists at the original path now; it was left alone.
    OriginalExists,
    FolderMissing,
    /// The Trash no longer holds it.
    NotInTrash,
    /// Something else sits at the Trash location now.
    Replaced,
    UnknownLocation,
    Failed {
        detail: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiLargeFileUndoResult {
    pub file_id: String,
    pub path: String,
    pub outcome: FfiUndoOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiLargeFilesUndoReport {
    pub results: Vec<FfiLargeFileUndoResult>,
    pub restored_count: u64,
}

pub(super) fn file_record(id: &str, file: &LargeFile) -> FfiLargeFile {
    FfiLargeFile {
        id: id.to_string(),
        path: file.path.to_string_lossy().into_owned(),
        name: file.name.clone(),
        group: file.group.id().to_string(),
        reason: file.reason.clone(),
        frees_bytes: file.frees.bytes,
        frees_is_exact: file.frees.exact,
        matched_app_path: file
            .matched_app
            .as_ref()
            .map(|p| p.to_string_lossy().into_owned()),
    }
}

impl From<UndoOutcome> for FfiUndoOutcome {
    fn from(outcome: UndoOutcome) -> Self {
        match outcome {
            UndoOutcome::Restored => Self::Restored,
            UndoOutcome::OriginalExists => Self::OriginalExists,
            UndoOutcome::FolderMissing => Self::FolderMissing,
            UndoOutcome::NotInTrash => Self::NotInTrash,
            UndoOutcome::Replaced => Self::Replaced,
            UndoOutcome::UnknownLocation => Self::UnknownLocation,
            UndoOutcome::Failed(detail) => Self::Failed { detail },
        }
    }
}

pub(super) fn cause(failure: Option<&Failure>) -> Option<FfiFailureCause> {
    failure.map(|f| f.cause.into())
}
