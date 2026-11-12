//! Large Files: the user's own big files, found read-only and moved to the
//! Trash only when picked (docs/internals/clean.md, "Large Files").
//!
//! Groups so far: Copies (one recording saved twice), Installers, Only copy on this Mac, Other large files and
//! Remove these elsewhere (listed, never moved from here). Moves reuse Clean's pieces (path
//! fingerprint, saved "always keep" rules, home volume, protected paths,
//! the native Trash route, failure causes) in [`move_run`]; nothing here
//! deletes for good, and [`undo`] only puts a trashed item back.

pub mod app_version;
mod copies;
mod discovery;
pub mod elsewhere;
mod installers;
pub mod last_used;
pub mod move_run;
mod name_match;
pub mod undo;
pub mod video_facts;

use std::path::PathBuf;
use std::time::SystemTime;

use super::fs_safe::PathFingerprint;
use super::private_size::PrivateSize;

pub use copies::{CopyOf, KeepRule, COPY_MAX_LENGTH_GAP_MS, COPY_MIN_BYTES};
pub use discovery::{
    scan_home, HomeScan, HomeWalk, Limits, ONLY_COPY_MIN_BYTES, OTHER_MIN_AGE_DAYS,
    OTHER_MIN_BYTES, VIDEO_EXTENSIONS,
};
pub use elsewhere::{scan_elsewhere, AppLibrary, ElsewhereRoots, LIBRARIES};
pub use installers::{
    scan_installers, scan_installers_above, InstallerRoots, InstallerScan, INSTALLER_EXTENSIONS,
    INSTALLER_MIN_BYTES,
};

/// How sure a removal is (GR1), in the order the groups are drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    /// One recording saved twice; the original stays (CP1).
    Copies,
    Installers,
    OnlyCopy,
    OtherLarge,
    /// App-managed data and files that free nothing (OW1, FN1): listed, and
    /// never movable through the preview.
    Elsewhere,
}

impl Group {
    /// The stable identifier the clients key on.
    pub fn id(self) -> &'static str {
        match self {
            Self::Copies => "copies",
            Self::Installers => "installers",
            Self::OnlyCopy => "only_copy",
            Self::OtherLarge => "other_large",
            Self::Elsewhere => "elsewhere",
        }
    }

    /// False for a group this tab only lists: the preview refuses its files.
    pub fn movable(self) -> bool {
        self != Self::Elsewhere
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
    /// What only group 5 knows; `None` in every other group.
    pub elsewhere: Option<Elsewhere>,
    /// What only group 1 knows: the file that stays; `None` elsewhere.
    pub copy: Option<CopyOf>,
}

/// The facts behind a "Remove these elsewhere" row. `LargeFile::matched_app`
/// is the owning app's bundle, when it is installed.
#[derive(Debug, Clone)]
pub struct Elsewhere {
    /// "Virtual machine", "Screen recording", "Installer" or "File".
    pub kind: String,
    /// The app that owns the item, or whose library holds the clone partner.
    pub app_name: Option<String>,
    pub app_bundle_id: Option<String>,
    /// The owning app was running when the scan looked (an unknown answer is
    /// `false`; a later move would ask again).
    pub app_running: bool,
    /// An image that shows the item (UTM's screenshot, a Cap screenshot).
    pub preview: Option<PathBuf>,
    /// Set for a file that frees nothing (FN1): the size it shares.
    pub shared_bytes: Option<u64>,
    /// With `shared_bytes`: how the clone's partner in a known library is
    /// named ("UTM's Windows VM"); `None` when it was not found there.
    pub shared_with: Option<String>,
    /// The newest change inside the item, or the file's own date.
    pub last_changed: Option<SystemTime>,
}

/// A large file whose private bytes are 0 (every block shared with a clone):
/// no group offers it, group 5 explains it (FN1).
#[derive(Debug, Clone)]
pub struct FreesNothing {
    pub path: PathBuf,
    /// Allocated bytes, all shared.
    pub allocated: u64,
    pub modified: Option<SystemTime>,
    pub fingerprint: PathFingerprint,
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod discovery_tests;

#[cfg(test)]
mod elsewhere_tests;

#[cfg(test)]
mod copies_tests;
