//! Undo for a Large Files move: puts just-trashed items back where they
//! were. Nothing is overwritten and nothing is created: an item goes back
//! only when its original path is still free, its folder still exists and
//! the item in the Trash is still the file that moved. Each item answers
//! for itself.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use crate::clean::fs_safe::PathFingerprint;

/// One moved item, as the move recorded it.
#[derive(Debug, Clone)]
pub struct UndoItem {
    pub original: PathBuf,
    pub trashed: PathBuf,
    /// The file as it was when it moved.
    pub fingerprint: PathFingerprint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UndoOutcome {
    Restored,
    /// Something exists at the original path now; it is left alone.
    OriginalExists,
    /// The folder it came from is gone.
    FolderMissing,
    /// The Trash no longer holds it (emptied, or taken out by hand).
    NotInTrash,
    /// Something else sits at the Trash location now.
    Replaced,
    /// The Trash route did not say where the item went.
    UnknownLocation,
    Failed(String),
}

/// Puts each item back, in order. Returns one outcome per item.
pub fn undo_moves(items: &[UndoItem]) -> Vec<UndoOutcome> {
    items.iter().map(undo_one).collect()
}

fn undo_one(item: &UndoItem) -> UndoOutcome {
    if item.trashed.as_os_str().is_empty() || !in_a_trash(&item.trashed) {
        return UndoOutcome::UnknownLocation;
    }
    let Ok(trashed_meta) = fs::symlink_metadata(&item.trashed) else {
        return UndoOutcome::NotInTrash;
    };
    if PathFingerprint::of(&trashed_meta) != item.fingerprint {
        return UndoOutcome::Replaced;
    }
    match item.original.parent().map(fs::symlink_metadata) {
        Some(Ok(parent)) if parent.is_dir() => {}
        _ => return UndoOutcome::FolderMissing,
    }
    // Also catches a symlink at the original path: it is something.
    if fs::symlink_metadata(&item.original).is_ok() {
        return UndoOutcome::OriginalExists;
    }
    match rename_without_replacing(&item.trashed, &item.original) {
        Ok(()) => UndoOutcome::Restored,
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => UndoOutcome::OriginalExists,
        Err(error) => UndoOutcome::Failed(error.to_string()),
    }
}

/// Only paths macOS keeps in a Trash: `~/.Trash` or a volume's `.Trashes`.
fn in_a_trash(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .any(|c| c.as_os_str() == ".Trash" || c.as_os_str() == ".Trashes")
}

/// `rename` that fails when `to` exists, so a file created between the check
/// and the move is never replaced.
#[cfg(target_os = "macos")]
fn rename_without_replacing(from: &Path, to: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let cstring = |path: &Path| {
        CString::new(path.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path holds a NUL byte"))
    };
    let (from, to) = (cstring(from)?, cstring(to)?);
    // SAFETY: both are valid NUL-terminated paths that outlive the call.
    let rc = unsafe { libc::renamex_np(from.as_ptr(), to.as_ptr(), libc::RENAME_EXCL) };
    match rc {
        0 => Ok(()),
        _ => Err(io::Error::last_os_error()),
    }
}

#[cfg(not(target_os = "macos"))]
fn rename_without_replacing(from: &Path, to: &Path) -> io::Result<()> {
    match fs::symlink_metadata(to) {
        Ok(_) => Err(io::ErrorKind::AlreadyExists.into()),
        Err(_) => fs::rename(from, to),
    }
}
