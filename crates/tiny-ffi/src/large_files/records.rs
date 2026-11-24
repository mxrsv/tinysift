//! Records Swift sees for Large Files. Paths cross only outward: Swift names
//! file IDs, preview IDs and move IDs, never a path to act on.

use std::time::{SystemTime, UNIX_EPOCH};

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
    /// The app bundle the file was matched to, for its icon. Group 5: the
    /// app that owns the item, when it is installed.
    pub matched_app_path: Option<String>,
    /// Group 5 only (OW1, FN1, LV-L6): what the item is ("Virtual machine",
    /// "Virtual machine disk", "Screen recording", "Disk image", "Device
    /// backup", "Cloud-synced copy"; "Installer" or "File" for one that frees
    /// nothing).
    #[uniffi(default = None)]
    pub kind: Option<String>,
    /// Group 5: the owning app's name, and for a file that frees nothing the
    /// app that holds its clone partner.
    #[uniffi(default = None)]
    pub app_name: Option<String>,
    #[uniffi(default = None)]
    pub app_bundle_id: Option<String>,
    /// Group 5: whether the owning app was running at the scan.
    #[uniffi(default = false)]
    pub app_running: bool,
    /// Group 5: an image of the item (a VM's screenshot), when the app keeps one.
    #[uniffi(default = None)]
    pub preview_path: Option<String>,
    /// Group 5, a file that frees nothing: the size it shares with its clone.
    #[uniffi(default = None)]
    pub shared_bytes: Option<u64>,
    /// With `shared_bytes`: the clone's partner when it sits in a known app
    /// library ("UTM's Windows VM"); `None` when it was not found there.
    #[uniffi(default = None)]
    pub shared_with: Option<String>,
    /// Group 5: the newest change in the item, in seconds since 1970.
    #[uniffi(default = None)]
    pub last_changed: Option<i64>,
    /// Group 1 (CP1): the file that stays when this copy goes.
    #[uniffi(default = None)]
    pub keeper_path: Option<String>,
    #[uniffi(default = None)]
    pub keeper_bytes: Option<u64>,
    /// Why that file stays: "mp4", "mov" or "larger".
    #[uniffi(default = None)]
    pub keep_rule: Option<String>,
    /// Group 1: this file's length in milliseconds, its picture size and its
    /// audio-track count; the file that stays has the same size and count.
    #[uniffi(default = None)]
    pub duration_ms: Option<u64>,
    #[uniffi(default = None)]
    pub keeper_duration_ms: Option<u64>,
    #[uniffi(default = None)]
    pub width: Option<u32>,
    #[uniffi(default = None)]
    pub height: Option<u32>,
    #[uniffi(default = None)]
    pub audio_tracks: Option<u32>,
    /// What the page's lens chips filter by (LV-L5), whatever the group:
    /// "video", "installer" (an installer or a disk image), "archive",
    /// "virtual_machine", "audio" or "other".
    #[uniffi(default = "other")]
    pub lens: String,
    /// Groups 3 and 4: the site the file was downloaded from (its where-from
    /// attribute, host only); the reason already names it.
    #[uniffi(default = None)]
    pub downloaded_from: Option<String>,
    /// Group 1, an archive: the unpacked folder (or single file) beside it
    /// that holds every file it lists, at the same sizes.
    #[uniffi(default = None)]
    pub extracted_path: Option<String>,
    /// With `extracted_path`: how many files, folders and links it lists.
    #[uniffi(default = None)]
    pub extracted_entries: Option<u64>,
    /// True for a row the size filter for groups 3 and 4 hides under its
    /// minimum (LV-L5): every group 3 and 4 row, by `frees_bytes`, and a
    /// group 5 file that frees nothing found by the home walk, by
    /// `shared_bytes` (FN1). False for every other row; an installer that
    /// frees nothing keeps group 2's 100 MB.
    #[uniffi(default = false)]
    pub follows_size_filter: bool,
}

/// What a Large Files scan lists (`large_files_scan_with`).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiLargeFilesScanOptions {
    /// A folder inside the home to list, as `large_files_scan_in`; `None`
    /// lists the whole home.
    #[uniffi(default = None)]
    pub root: Option<String>,
    /// Groups 3 and 4 list files that free at least this much (the size
    /// filter, LV-L5). At least `FfiLargeFilesSizeFilters::floor_bytes`,
    /// else `InvalidInput`. Other groups keep their own floors.
    #[uniffi(default = 500000000)]
    pub groups_3_4_min_bytes: u64,
}

/// The sizes the Large Files page offers (LV-L5), so the app types none.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiLargeFilesSizeFilters {
    /// Nothing smaller is listed by any group; groups 1 and 2 list from it.
    pub floor_bytes: u64,
    /// The filter's choices for groups 3 and 4, smallest first.
    pub choices_bytes: Vec<u64>,
    /// The choice groups 3 and 4 use until the user picks another.
    pub default_bytes: u64,
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

/// Seconds since 1970, negative before it.
fn unix_seconds(time: SystemTime) -> i64 {
    match time.duration_since(UNIX_EPOCH) {
        Ok(after) => i64::try_from(after.as_secs()).unwrap_or(i64::MAX),
        Err(before) => -i64::try_from(before.duration().as_secs()).unwrap_or(i64::MAX),
    }
}

pub(super) fn file_record(id: &str, file: &LargeFile) -> FfiLargeFile {
    let facts = file.elsewhere.as_ref();
    let copy = file.copy.as_ref();
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
        kind: facts.map(|e| e.kind.clone()),
        app_name: facts.and_then(|e| e.app_name.clone()),
        app_bundle_id: facts.and_then(|e| e.app_bundle_id.clone()),
        app_running: facts.is_some_and(|e| e.app_running),
        preview_path: facts
            .and_then(|e| e.preview.as_ref())
            .map(|p| p.to_string_lossy().into_owned()),
        shared_bytes: facts.and_then(|e| e.shared_bytes),
        shared_with: facts.and_then(|e| e.shared_with.clone()),
        last_changed: facts.and_then(|e| e.last_changed).map(unix_seconds),
        keeper_path: copy.map(|c| c.keeper.to_string_lossy().into_owned()),
        keeper_bytes: copy.map(|c| c.keeper_bytes),
        keep_rule: copy.map(|c| c.keep_rule.id().to_string()),
        duration_ms: copy.map(|c| c.facts.duration_ms),
        keeper_duration_ms: copy.map(|c| c.keeper_duration_ms),
        width: copy.map(|c| c.facts.width),
        height: copy.map(|c| c.facts.height),
        audio_tracks: copy.map(|c| c.facts.audio_tracks),
        lens: file.lens.id().to_string(),
        downloaded_from: file.downloaded_from.clone(),
        extracted_path: file
            .extracted
            .as_ref()
            .map(|e| e.beside.to_string_lossy().into_owned()),
        extracted_entries: file.extracted.as_ref().map(|e| e.entries),
        follows_size_filter: file.follows_size_filter(),
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
