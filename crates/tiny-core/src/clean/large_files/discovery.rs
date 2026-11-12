//! Groups 3 and 4 of Large Files (GR1): one read-only walk of the user's
//! home, metadata only, nothing hashed. A video of 500 MB or more is
//! "Only copy on this Mac"; any other file of 500 MB or more that has not
//! changed for 90 days is "Other large files" (GR3).
//!
//! What the walk never enters or lists (NL1): anything outside the home
//! (symlinks are not followed), `~/Library` (app libraries are task 10's),
//! app bundles and package libraries, VM bundles, hidden folders (tool
//! caches, `.Trash`), Smart Clean's build-output folder names, a folder with
//! a `.git` inside (a git work tree), and an iCloud placeholder.
//!
//! The existing CLI walker (`scan::walker`) did not fit: it collects every
//! file of a root into memory, stops at a depth of 12, aborts the whole walk
//! on one unreadable folder and has no cancellation or progress.

use std::collections::HashSet;
use std::fs::{self, Metadata};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, SystemTime};

use crate::clean::fs_safe::PathFingerprint;
use crate::clean::private_size::private_allocated_bytes;
use crate::progress::{report, ProgressCallback};

use super::last_used::LastUsed;
use super::{Group, LargeFile};

/// 500 MB, decimal (GR3), judged on private bytes like every threshold here.
pub const OTHER_MIN_BYTES: u64 = 500_000_000;
/// "Only copy" has no threshold of its own in the spec: it takes group 4's.
pub const ONLY_COPY_MIN_BYTES: u64 = OTHER_MIN_BYTES;
/// Not changed for this many days (GR1 group 4).
pub const OTHER_MIN_AGE_DAYS: u64 = 90;
/// What makes a file a video: the one kind group 3 explains so far.
pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mov", "mkv", "m4v", "avi", "webm"];

const SECONDS_PER_DAY: u64 = 86_400;
/// How many entries pass between two progress reports.
const PROGRESS_EVERY: u64 = 2_000;
/// `SF_DATALESS` in `st_flags`: the file's bytes are in iCloud, not on disk.
const SF_DATALESS: u32 = 0x4000_0000;

/// Folder names Smart Clean owns as build output or dependency caches
/// (`scan::walker`'s own list plus two Xcode/CocoaPods ones).
const BUILD_OUTPUT_NAMES: &[&str] = &[
    "node_modules",
    "vendor",
    "target",
    "build",
    "dist",
    "__pycache__",
    "venv",
    "DerivedData",
    "Pods",
];

/// A folder with one of these extensions is one thing, not a folder of files.
const OPAQUE_EXTENSIONS: &[&str] = &[
    // App bundles and their relatives.
    "app",
    "appex",
    "framework",
    "plugin",
    "xpc",
    // Package libraries (Photos, Music, TV, Final Cut, iMovie).
    "photoslibrary",
    "photolibrary",
    "aplibrary",
    "migratedphotolibrary",
    "musiclibrary",
    "tvlibrary",
    "fcpbundle",
    "imovielibrary",
    "theater",
    // Virtual machines and disk bundles an app manages.
    "utm",
    "vmwarevm",
    "pvm",
    "sparsebundle",
];

/// Home-relative folders that are another app's library (Music's media, the
/// TV app's media). `Library` itself is skipped by name at the home level.
const HOME_LIBRARY_FOLDERS: &[&str] = &["Library", "Music/Music", "Movies/TV"];

/// The numbers a scan judges by, injectable so tests need no 500 MB files.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub only_copy_min_bytes: u64,
    pub other_min_bytes: u64,
    pub other_min_age_days: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            only_copy_min_bytes: ONLY_COPY_MIN_BYTES,
            other_min_bytes: OTHER_MIN_BYTES,
            other_min_age_days: OTHER_MIN_AGE_DAYS,
        }
    }
}

/// Everything one home walk needs besides cancellation and progress.
pub struct HomeWalk<'a> {
    pub home: &'a Path,
    pub limits: Limits,
    pub last_used: &'a dyn LastUsed,
    /// "Today" for ages, so tests fix it.
    pub now: SystemTime,
    /// Files another group already claimed (installers), left out here.
    pub claimed: &'a HashSet<PathBuf>,
}

#[derive(Debug, Clone, Default)]
pub struct HomeScan {
    /// Groups 3 and 4 together, largest first by what each frees.
    pub files: Vec<LargeFile>,
    /// Folders that exist but could not be listed.
    pub unreadable: Vec<PathBuf>,
    /// Files looked at, for the timing report.
    pub visited: u64,
}

/// Reads only: folder listings and file metadata, plus `last_used` for a
/// file that already passed every other test. A cancelled walk returns what
/// it had; the caller checks the flag.
pub fn scan_home(
    walk: &HomeWalk<'_>,
    cancel: Option<&AtomicBool>,
    progress: ProgressCallback<'_>,
) -> HomeScan {
    let mut scan = HomeScan::default();
    let mut seen_inodes: HashSet<(u64, u64)> = HashSet::new();
    let mut pending = vec![walk.home.to_path_buf()];
    let min_bytes = walk
        .limits
        .only_copy_min_bytes
        .min(walk.limits.other_min_bytes);
    while let Some(dir) = pending.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries.flatten().collect::<Vec<_>>(),
            Err(error) => {
                if error.kind() != std::io::ErrorKind::NotFound {
                    scan.unreadable.push(dir);
                }
                continue;
            }
        };
        // A git work tree is the developer's; the home itself may hold dotfiles.
        if dir != walk.home && entries.iter().any(|e| e.file_name() == ".git") {
            continue;
        }
        for entry in entries {
            if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
                return scan;
            }
            let name = entry.file_name();
            let name = name.to_string_lossy();
            if name.starts_with('.') {
                continue;
            }
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            let path = entry.path();
            scan.visited += 1;
            if scan.visited % PROGRESS_EVERY == 0 {
                report(progress, "scan", scan.visited, None, &dir.to_string_lossy());
            }
            if meta.is_dir() {
                if !skips_folder(&path, &name, walk.home) && !is_dataless(&meta) {
                    pending.push(path);
                }
            } else if meta.is_file() && allocated(&meta) >= min_bytes && !is_dataless(&meta) {
                if let Some(file) = candidate(&path, &meta, walk, &mut seen_inodes) {
                    scan.files.push(file);
                }
            }
        }
    }
    scan.files
        .sort_by(|a, b| b.frees.bytes.cmp(&a.frees.bytes).then(a.path.cmp(&b.path)));
    report(progress, "scan", scan.visited, None, "Done");
    scan
}

fn allocated(meta: &Metadata) -> u64 {
    meta.blocks().saturating_mul(512)
}

/// True for an iCloud placeholder (NL1): listing it would offer to move a
/// file whose bytes are not on this Mac.
pub(crate) fn is_dataless(meta: &Metadata) -> bool {
    flags_are_dataless(flags_of(meta))
}

pub(crate) fn flags_are_dataless(flags: u32) -> bool {
    flags & SF_DATALESS != 0
}

#[cfg(target_os = "macos")]
fn flags_of(meta: &Metadata) -> u32 {
    std::os::macos::fs::MetadataExt::st_flags(meta)
}

#[cfg(not(target_os = "macos"))]
fn flags_of(_meta: &Metadata) -> u32 {
    0
}

/// True when the walk does not go into this folder (NL1).
fn skips_folder(path: &Path, name: &str, home: &Path) -> bool {
    if BUILD_OUTPUT_NAMES.contains(&name) {
        return true;
    }
    if path
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| OPAQUE_EXTENSIONS.contains(&e.to_lowercase().as_str()))
    {
        return true;
    }
    path.strip_prefix(home)
        .is_ok_and(|rel| HOME_LIBRARY_FOLDERS.iter().any(|lib| rel == Path::new(lib)))
}

fn candidate(
    path: &Path,
    meta: &Metadata,
    walk: &HomeWalk<'_>,
    seen_inodes: &mut HashSet<(u64, u64)>,
) -> Option<LargeFile> {
    if walk.claimed.contains(path) {
        return None;
    }
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase);
    let video = extension
        .as_deref()
        .is_some_and(|e| VIDEO_EXTENSIONS.contains(&e));
    let (group, min_bytes) = match video {
        true => (Group::OnlyCopy, walk.limits.only_copy_min_bytes),
        false => (Group::OtherLarge, walk.limits.other_min_bytes),
    };
    let modified = meta.modified().ok()?;
    let reason = match group {
        Group::OnlyCopy => video_reason(days_between(modified, walk.now)),
        _ => other_reason(path, extension.as_deref(), modified, walk)?,
    };
    // A hard link counts once: its twin is the same blocks.
    if !seen_inodes.insert((meta.dev(), meta.ino())) {
        return None;
    }
    // A file that frees nothing is not offered here (task 10 and FN1 own it).
    let frees = private_allocated_bytes(path).ok()?;
    if frees.bytes == 0 || frees.bytes < min_bytes {
        return None;
    }
    let stem = path.file_stem()?.to_str()?;
    Some(LargeFile {
        path: path.to_path_buf(),
        name: stem.to_string(),
        group,
        reason,
        frees,
        matched_app: None,
        fingerprint: PathFingerprint::of(meta),
    })
}

fn video_reason(days: u64) -> String {
    format!("Video · changed {}", ago(days))
}

/// Group 4's reason, `None` while the file is too recent. Spotlight's date
/// wins only when it is newer, and then the sentence says "used", because
/// that is the fact it checked (LR1).
fn other_reason(
    path: &Path,
    extension: Option<&str>,
    modified: SystemTime,
    walk: &HomeWalk<'_>,
) -> Option<String> {
    let min_days = walk.limits.other_min_age_days;
    // Spotlight is asked only once the modification date alone passes.
    if days_between(modified, walk.now) < min_days {
        return None;
    }
    let used = walk
        .last_used
        .last_used(path)
        .filter(|used| *used > modified);
    let (days, verb) = match used {
        Some(used) => (days_between(used, walk.now), "used"),
        None => (days_between(modified, walk.now), "changed"),
    };
    (days >= min_days).then(|| format!("{} · not {verb} for {days} days", kind_of(extension)))
}

fn days_between(then: SystemTime, now: SystemTime) -> u64 {
    now.duration_since(then).unwrap_or(Duration::ZERO).as_secs() / SECONDS_PER_DAY
}

fn ago(days: u64) -> String {
    match days {
        0 => "today".into(),
        1 => "1 day ago".into(),
        n => format!("{n} days ago"),
    }
}

/// What the extension says the file is; the extension itself otherwise.
fn kind_of(extension: Option<&str>) -> String {
    let kind = match extension {
        Some("dmg" | "iso" | "img" | "sparseimage") => "Disk image",
        Some("pkg" | "mpkg" | "xip") => "Installer package",
        Some("zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "7z" | "rar") => "Archive",
        Some("vmdk" | "vdi" | "qcow2" | "vhd" | "vhdx") => "Virtual disk",
        Some("wav" | "aif" | "aiff" | "flac" | "mp3" | "m4a" | "aac") => "Audio",
        Some("psd" | "tif" | "tiff" | "raw" | "dng") => "Image",
        Some("pdf") => "PDF",
        Some(other) if !other.is_empty() => return format!("{} file", other.to_uppercase()),
        _ => "File",
    };
    kind.to_string()
}
