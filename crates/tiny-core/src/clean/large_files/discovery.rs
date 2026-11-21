//! Groups 3 and 4 of Large Files (GR1, LV-L6), with the group 1 and group 5
//! items the same walk meets: one read-only walk of the user's home,
//! metadata only, nothing hashed. A file of a kind group 3 holds (video,
//! audio, disk image, archive, database dump) is "Only copy on this Mac";
//! any other file that has not changed for 90 days is "Other large files";
//! neither when Smart Clean's Old Downloads already lists it (LV-L5). Both groups
//! list from `Limits::only_copy_min_bytes` / `other_min_bytes` (500 MB by
//! default, the size filter's choice, never under the 100 MB scan floor).
//!
//! What the walk never enters or lists (NL1, LV-L5): anything outside the
//! home (symlinks are not followed), `~/Library` and hidden folders (their
//! app data is read only through [`super::elsewhere`] and the known
//! locations of [`super::owned`]), app bundles, packages (what macOS calls
//! one, plus a fixed list for those whose app is not installed) and package
//! libraries, another volume's mount point, Smart Clean's build-output
//! folder names, a folder with a `.git` inside (a git work tree), and an
//! iCloud placeholder. A virtual machine or disk image bundle whose format
//! marker is inside, and a VirtualBox machine folder, are not entered
//! either: each becomes one group 5 item.
//!
//! The existing CLI walker (`scan::walker`) did not fit: it collects every
//! file of a root into memory, stops at a depth of 12, aborts the whole walk
//! on one unreadable folder and has no cancellation or progress.

use std::collections::HashSet;
use std::fs::{self, Metadata};
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use crate::clean::fs_safe::PathFingerprint;
use crate::clean::private_size::private_allocated_bytes;
use crate::options::CleanOptions;
use crate::progress::{report, ProgressCallback};

use super::archive_beside::{is_checked_archive, unpacked_beside};
use super::copies::{find_pairs, CopyOf, VideoEntry, COPY_MIN_BYTES};
use super::last_used::LastUsed;
use super::lens::Lens;
use super::owned::{owner_of_folder, virtualbox_vm, OwnedItem};
use super::package_dir::is_package;
use super::row_words::{days_between, idle_days, only_copy_kind, only_copy_reason, other_reason};
use super::where_from::downloaded_from;
use super::{FreesNothing, Group, LargeFile};

/// 100 MB, decimal: nothing smaller is listed by any group (LV-L5).
pub const SCAN_FLOOR_BYTES: u64 = 100_000_000;
/// The size filter's choices for groups 3 and 4 (LV-L5): 100 MB, 500 MB, 1 GB.
pub const SIZE_FILTER_BYTES: [u64; 3] = [SCAN_FLOOR_BYTES, 500_000_000, 1_000_000_000];
/// Groups 3 and 4 list from 500 MB unless the filter says otherwise (GR3).
pub const DEFAULT_SIZE_FILTER_BYTES: u64 = 500_000_000;
/// 500 MB, decimal (GR3), judged on private bytes like every threshold here.
pub const OTHER_MIN_BYTES: u64 = DEFAULT_SIZE_FILTER_BYTES;
/// "Only copy" has no threshold of its own in the spec: it takes group 4's.
pub const ONLY_COPY_MIN_BYTES: u64 = OTHER_MIN_BYTES;
/// Not changed for this many days (GR1 group 4).
pub const OTHER_MIN_AGE_DAYS: u64 = 90;
/// What makes a file a video.
pub const VIDEO_EXTENSIONS: &[&str] = &["mp4", "mov", "mkv", "m4v", "avi", "webm"];
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
    // Virtual machines and disk bundles an app manages: one group 5 item
    // when their marker is inside (`owned`), never entered either way.
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
    pub copy_min_bytes: u64,
    pub only_copy_min_bytes: u64,
    pub other_min_bytes: u64,
    pub other_min_age_days: u64,
    /// Smart Clean's Old Downloads lists every file directly in
    /// `~/Downloads` unused for more than this many days; such a file is
    /// that category's, not group 4's (LV-L5). `None` claims nothing.
    pub old_downloads_days: Option<u64>,
    /// A group 5 item an app owns that frees less is not listed: the floor.
    pub owned_min_bytes: u64,
    /// A cloud app's local copy is listed in group 5 from this size, the
    /// groups 3 and 4 default; the size filter does not change it.
    pub cloud_min_bytes: u64,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            copy_min_bytes: COPY_MIN_BYTES,
            only_copy_min_bytes: ONLY_COPY_MIN_BYTES,
            other_min_bytes: OTHER_MIN_BYTES,
            other_min_age_days: OTHER_MIN_AGE_DAYS,
            // The days the app's Smart Clean scans with.
            old_downloads_days: Some(CleanOptions::default().idle_days),
            owned_min_bytes: SCAN_FLOOR_BYTES,
            cloud_min_bytes: DEFAULT_SIZE_FILTER_BYTES,
        }
    }
}

impl Limits {
    /// The defaults with groups 3 and 4 listing from `min_bytes` (the size
    /// filter); `None` under the scan floor.
    pub fn with_size_filter(min_bytes: u64) -> Option<Self> {
        (min_bytes >= SCAN_FLOOR_BYTES).then(|| Self {
            only_copy_min_bytes: min_bytes,
            other_min_bytes: min_bytes,
            ..Self::default()
        })
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
    /// Files of the size a group lists whose blocks are all shared (FN1).
    pub frees_nothing: Vec<FreesNothing>,
    /// Virtual machines and disk image bundles the walk met (group 5).
    pub owned: Vec<OwnedItem>,
}

/// Reads only: folder listings and file metadata, plus `last_used` for a
/// file that already passed every other test. A cancelled walk returns what
/// it had; the caller checks the flag.
pub fn scan_home(
    walk: &HomeWalk<'_>,
    cancel: Option<&AtomicBool>,
    progress: ProgressCallback<'_>,
) -> HomeScan {
    // A home that cannot be read has no device; every listing then fails alike.
    let device = fs::metadata(walk.home).map_or(0, |meta| meta.dev());
    scan_home_on(walk, device, cancel, progress)
}

/// `scan_home` on the volume `device`: a folder on another one (a mounted
/// disk or share inside the home) is left alone (NL1). Split so a test can
/// name a device without mounting one.
pub(super) fn scan_home_on(
    walk: &HomeWalk<'_>,
    device: u64,
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
    // Videos big enough to be half of a Copies pair (CP1), and archives
    // big enough to be a Copies row (LV-L6).
    let mut videos: Vec<VideoEntry> = Vec::new();
    let mut archives: Vec<PathBuf> = Vec::new();
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
        // A VirtualBox machine is one group 5 item, never its disk files.
        if dir != walk.home {
            let names: Vec<(String, bool)> = entries
                .iter()
                .map(|e| {
                    let is_file = e.file_type().is_ok_and(|t| t.is_file());
                    (e.file_name().to_string_lossy().into_owned(), is_file)
                })
                .collect();
            if let Some(item) = virtualbox_vm(&dir, &names) {
                scan.owned.push(item);
                continue;
            }
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
                if meta.dev() != device || is_dataless(&meta) {
                    continue;
                }
                if let Some(item) = owner_of_folder(&path) {
                    scan.owned.push(item);
                } else if !skips_folder(&path, &name, walk.home) {
                    pending.push(path);
                }
            } else if meta.is_file() && !is_dataless(&meta) {
                if allocated(&meta) >= walk.limits.copy_min_bytes {
                    if is_video(&path) {
                        videos.push(VideoEntry {
                            path: path.clone(),
                            bytes: meta.len(),
                            fingerprint: PathFingerprint::of(&meta),
                        });
                    } else if is_checked_archive(&path) && !walk.claimed.contains(&path) {
                        archives.push(path.clone());
                    }
                }
                if allocated(&meta) < min_bytes {
                    continue;
                }
                let found = candidate(
                    &path,
                    &meta,
                    walk,
                    &mut seen_inodes,
                    &mut scan.frees_nothing,
                );
                scan.files.extend(found);
            }
        }
    }
    add_copies(&mut scan, &videos, walk.limits.copy_min_bytes);
    add_archive_copies(&mut scan, &archives, walk.limits.copy_min_bytes, cancel);
    scan.files
        .sort_by(|a, b| b.frees.bytes.cmp(&a.frees.bytes).then(a.path.cmp(&b.path)));
    report(progress, "scan", scan.visited, None, "Done");
    scan
}

/// Group 1 (CP1): a pair's row leaves groups 3 and 4, which it would
/// otherwise sit in; the file that stays is left wherever it was. A row
/// that frees less than `min_bytes` privately (or nothing: FN1 explains it)
/// is not offered.
fn add_copies(scan: &mut HomeScan, videos: &[VideoEntry], min_bytes: u64) {
    for pair in find_pairs(videos) {
        let Ok(frees) = private_allocated_bytes(&pair.row.path) else {
            continue;
        };
        if frees.bytes < min_bytes {
            continue;
        }
        let Some(stem) = pair.row.path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let keeper_name = pair
            .keeper
            .path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        scan.files.retain(|f| f.path != pair.row.path);
        scan.files.push(LargeFile {
            path: pair.row.path.clone(),
            name: stem.to_string(),
            group: Group::Copies,
            // Words only: the app builds the sentence with the numbers (LR1).
            reason: format!("Same length and resolution as {keeper_name}"),
            frees,
            matched_app: None,
            fingerprint: pair.row.fingerprint,
            elsewhere: None,
            extracted: None,
            lens: Lens::Video,
            downloaded_from: None,
            copy: Some(CopyOf {
                keeper: pair.keeper.path,
                keeper_bytes: pair.keeper.bytes,
                keeper_fingerprint: pair.keeper.fingerprint,
                keep_rule: pair.keep_rule,
                facts: pair.facts,
                keeper_duration_ms: pair.keeper_duration_ms,
            }),
        });
    }
}

/// Group 1 (LV-L6): an archive whose contents sit unpacked beside it leaves
/// groups 3 and 4. It needs `min_bytes` of private bytes, as a video copy.
/// Its unpacked files leave groups 3 and 4 too: the archive holds a copy of
/// each, so none is "the only copy", and the archive's row explains them.
/// A file in the folder that the archive does not list stays where it was.
fn add_archive_copies(
    scan: &mut HomeScan,
    archives: &[PathBuf],
    min_bytes: u64,
    cancel: Option<&AtomicBool>,
) {
    for path in archives {
        if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return;
        }
        let Some((extracted, unpacked)) = unpacked_beside(path) else {
            continue;
        };
        let (Ok(meta), Ok(frees)) = (fs::symlink_metadata(path), private_allocated_bytes(path))
        else {
            continue;
        };
        if frees.bytes < min_bytes {
            continue;
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let beside = extracted
            .beside
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        let unpacked: HashSet<PathBuf> = unpacked.into_iter().collect();
        scan.files.retain(|f| {
            f.path != *path
                && !(matches!(f.group, Group::OnlyCopy | Group::OtherLarge)
                    && unpacked.contains(&f.path))
        });
        scan.files.push(LargeFile {
            path: path.clone(),
            name: stem.to_string(),
            group: Group::Copies,
            reason: format!("Its contents sit unpacked beside it in {beside}"),
            frees,
            matched_app: None,
            fingerprint: PathFingerprint::of(&meta),
            elsewhere: None,
            copy: None,
            extracted: Some(extracted),
            lens: Lens::Archive,
            downloaded_from: downloaded_from(path),
        });
    }
}

fn is_video(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| VIDEO_EXTENSIONS.contains(&e.to_lowercase().as_str()))
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
        || is_package(path)
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
    frees_nothing: &mut Vec<FreesNothing>,
) -> Option<LargeFile> {
    if walk.claimed.contains(path) {
        return None;
    }
    // A file that frees nothing is no group's row; group 5 explains it (FN1).
    // Judged before the age test: its size and that it frees 0 are the facts.
    // A hard-linked file frees 0 as well: its other link keeps the blocks.
    let frees = private_allocated_bytes(path).ok()?;
    if frees.bytes == 0 {
        if seen_inodes.insert((meta.dev(), meta.ino())) {
            frees_nothing.push(FreesNothing {
                path: path.to_path_buf(),
                allocated: allocated(meta),
                modified: meta.modified().ok(),
                fingerprint: PathFingerprint::of(meta),
                follows_size_filter: true,
            });
        }
        return None;
    }
    let extension = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase);
    let modified = meta.modified().ok()?;
    // The where-from attribute is read only for a file that is listed.
    let (group, reason, from) = match only_copy_kind(extension.as_deref()) {
        Some(kind) => {
            if frees.bytes < walk.limits.only_copy_min_bytes {
                return None;
            }
            let days = days_between(modified, walk.now);
            // A kind is no more specific than Old Downloads' age (LV-L5 (3)).
            if old_downloads_owns(path, walk, days) {
                // Spotlight is asked only once the change date alone passes.
                let used = walk.last_used.last_used(path).filter(|u| *u > modified);
                if used.is_none_or(|u| old_downloads_owns(path, walk, days_between(u, walk.now))) {
                    return None;
                }
            }
            let from = downloaded_from(path);
            let reason = only_copy_reason(kind, from.as_deref(), days);
            (Group::OnlyCopy, reason, from)
        }
        None => {
            if frees.bytes < walk.limits.other_min_bytes {
                return None;
            }
            let (days, verb) = idle_days(
                path,
                modified,
                walk.now,
                walk.limits.other_min_age_days,
                walk.last_used,
            )?;
            if old_downloads_owns(path, walk, days) {
                return None;
            }
            let from = downloaded_from(path);
            let reason = other_reason(extension.as_deref(), from.as_deref(), days, verb);
            (Group::OtherLarge, reason, from)
        }
    };
    let stem = path.file_stem()?.to_str()?;
    Some(LargeFile {
        path: path.to_path_buf(),
        name: stem.to_string(),
        group,
        reason,
        frees,
        matched_app: None,
        fingerprint: PathFingerprint::of(meta),
        elsewhere: None,
        copy: None,
        extracted: None,
        lens: Lens::of_path(path),
        downloaded_from: from,
    })
}

/// True when Smart Clean's Old Downloads lists this group 3 or 4 file (LV-L5
/// (3): no Smart Clean category may already own its path; only a copy, an
/// installer or an unpacked archive is the more specific reason, and groups 1
/// and 2 are decided apart from this). That category lists every visible
/// regular file directly in `~/Downloads` neither changed nor opened for more
/// than its days; `days` is the same fact here (the newer of the change
/// date and Spotlight's last-used date), counted in whole days.
fn old_downloads_owns(path: &Path, walk: &HomeWalk<'_>, days: u64) -> bool {
    walk.limits
        .old_downloads_days
        .is_some_and(|limit| days > limit && path.parent() == Some(&walk.home.join("Downloads")))
}
