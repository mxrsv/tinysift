//! Large Files: the user's own big files, found read-only and moved to the
//! Trash only when picked (docs/internals/clean.md, "Large Files").
//!
//! Groups so far: Installers, Only copy on this Mac and Other large files. Moves reuse Clean's pieces (path
//! fingerprint, saved "always keep" rules, home volume, protected paths,
//! the native Trash route, failure causes) in [`move_run`]; nothing here
//! deletes for good, and [`undo`] only puts a trashed item back.

pub mod app_version;
mod discovery;
mod installers;
pub mod last_used;
pub mod move_run;
mod name_match;
pub mod undo;

use std::path::PathBuf;

use super::fs_safe::PathFingerprint;
use super::private_size::PrivateSize;

pub use discovery::{
    scan_home, HomeScan, HomeWalk, Limits, ONLY_COPY_MIN_BYTES, OTHER_MIN_AGE_DAYS,
    OTHER_MIN_BYTES, VIDEO_EXTENSIONS,
};
pub use installers::{
    scan_installers, scan_installers_above, InstallerRoots, InstallerScan, INSTALLER_EXTENSIONS,
    INSTALLER_MIN_BYTES,
};

/// How sure a removal is (GR1), in the order the groups are drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Installers,
    OnlyCopy,
    OtherLarge,
}

impl Group {
    /// The stable identifier the clients key on.
    pub fn id(self) -> &'static str {
        match self {
            Self::Installers => "installers",
            Self::OnlyCopy => "only_copy",
            Self::OtherLarge => "other_large",
        }
    }
}

/// One row of the Large Files list.
#[derive(Debug, Clone)]
pub struct LargeFile {
    pub path: PathBuf,
    /// How a person names it ("Cap installer"); the file name stays in `path`.
    pub name: String,
    pub group: Group,
    /// One sentence from a fact the scan checked (LR1).
    pub reason: String,
    /// What moving it frees once the Trash is emptied (FG-L1).
    pub frees: PrivateSize,
    /// The app bundle the file was matched to.
    pub matched_app: Option<PathBuf>,
    /// The file as the scan saw it: a move proceeds only while it still is.
    pub fingerprint: PathFingerprint,
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod discovery_tests;
