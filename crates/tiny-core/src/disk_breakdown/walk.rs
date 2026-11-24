//! The read-only walk of the Data volume: private bytes per item, each block once.
//!
//! A file counts its allocated blocks, a partly shared file its private size, and a full clone family or a hard-linked
//! inode once for the whole volume, in the item of the first group (then the first item) that holds a member. The walk
//! never follows a symlink, never leaves the Data volume's device and never opens a file: it lists folders and reads
//! metadata only.

use super::places::{Class, Layout, Place};
use super::Refusal;
use crate::clean::freed_size::{sharing_at, Sharing};
use crate::progress::{report, ProgressCallback};
use rayon::prelude::*;
use std::collections::{BTreeMap, HashMap};
use std::ffi::{CString, OsStr};
use std::fs::{self, File, Metadata};
use std::io;
use std::os::fd::AsRawFd;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

/// Folders deeper than this below the Data volume are not walked; they count as unreadable.
const MAX_DEPTH: usize = 192;
/// A progress event every this many entries.
const REPORT_EVERY: u64 = 25_000;

/// A hard-linked inode `(0, dev, ino)` or a full clone family `(1, dev, clone id)`.
type Key = (u8, u64, u64);
/// A listed folder: the open folder for the clone queries, and its files and folders with their metadata.
type Listing = (Option<File>, Vec<(PathBuf, Metadata)>);

/// What the walk found.
#[derive(Debug, Default)]
pub(crate) struct Walked {
    pub items: HashMap<Place, u64>,
    /// Folders that could not be read, by the path shown and why: how many were refused there.
    pub refused: BTreeMap<(Refusal, String), u64>,
}

/// Everything the parallel branches of one walk share.
#[derive(Clone, Copy)]
pub(crate) struct Walk<'a> {
    pub layout: &'a Layout,
    pub root: &'a Path,
    pub cancel: Option<&'a AtomicBool>,
    pub progress: ProgressCallback<'a>,
    /// The Data volume's file count, for progress.
    pub total: Option<u64>,
    pub entries: &'a AtomicU64,
    pub claims: &'a Mutex<HashMap<Key, (u64, Place)>>,
    pub device: u64,
}

impl Walk<'_> {
    fn cancelled(&self) -> bool {
        self.cancel.is_some_and(|flag| flag.load(Ordering::Acquire))
    }

    fn rel(&self, path: &Path) -> Vec<String> {
        path.strip_prefix(self.root)
            .map(|rest| {
                rest.iter()
                    .map(|part| part.to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default()
    }

    fn tick(&self) {
        let seen = self.entries.fetch_add(1, Ordering::Relaxed) + 1;
        if seen.is_multiple_of(REPORT_EVERY) {
            report(
                self.progress,
                "disk-breakdown",
                seen,
                self.total,
                "Measuring the disk",
            );
        }
    }
}

/// One branch of the walk; branches merge into their parent when they finish.
pub(crate) struct Branch<'a> {
    walk: Walk<'a>,
    found: Walked,
}

impl<'a> Branch<'a> {
    pub(crate) fn new(walk: Walk<'a>) -> Self {
        Self {
            walk,
            found: Walked::default(),
        }
    }

    pub(crate) fn finish(self) -> Walked {
        self.found
    }

    fn merge(&mut self, other: Walked) {
        for (place, bytes) in other.items {
            *self.found.items.entry(place).or_insert(0) += bytes;
        }
        for (key, count) in other.refused {
            *self.found.refused.entry(key).or_insert(0) += count;
        }
    }

    fn add(&mut self, place: &Place, bytes: u64) {
        if let Some(total) = self.found.items.get_mut(place) {
            *total = total.saturating_add(bytes);
        } else {
            self.found.items.insert(place.clone(), bytes);
        }
    }

    /// A folder whose children may belong to different items (the Data volume's root, `/Users`, the home, `~/Library`).
    pub(crate) fn walk_mixed(&mut self, dir: &Path, depth: usize) {
        let Some((listing, entries)) = self.list(dir) else {
            return;
        };
        let parent = self.walk.rel(dir);
        let mut folders: Vec<(PathBuf, Option<Place>)> = Vec::new();
        for (path, meta) in entries {
            let mut rel = parent.clone();
            rel.push(name_of(&path));
            match self.walk.layout.classify(&rel, meta.is_dir()) {
                Class::Item(place) if meta.is_dir() => folders.push((path, Some(place))),
                Class::Item(place) => {
                    let bytes = self.file(listing.as_ref(), &path, &meta, &place);
                    self.add(&place, bytes);
                }
                Class::Descend => folders.push((path, None)),
            }
        }
        let walk = self.walk;
        let results: Vec<Walked> = folders
            .par_iter()
            .map(|(path, place)| {
                let mut branch = Branch::new(walk);
                match place {
                    Some(place) => {
                        let bytes = branch.walk_item(path, place, depth + 1);
                        branch.add(place, bytes);
                    }
                    None => branch.walk_mixed(path, depth + 1),
                }
                branch.finish()
            })
            .collect();
        for found in results {
            self.merge(found);
        }
    }

    /// A folder wholly inside `place`: the bytes it holds that nothing else claims.
    fn walk_item(&mut self, dir: &Path, place: &Place, depth: usize) -> u64 {
        if depth > MAX_DEPTH {
            self.refuse(dir, None);
            return 0;
        }
        let Some((listing, entries)) = self.list(dir) else {
            return 0;
        };
        let mut bytes = 0u64;
        let mut folders = Vec::new();
        for (path, meta) in entries {
            if meta.is_dir() {
                folders.push(path);
            } else {
                bytes = bytes.saturating_add(self.file(listing.as_ref(), &path, &meta, place));
            }
        }
        drop(listing);
        let walk = self.walk;
        let results: Vec<(u64, Walked)> = folders
            .par_iter()
            .map(|path| {
                let mut branch = Branch::new(walk);
                let bytes = branch.walk_item(path, place, depth + 1);
                (bytes, branch.finish())
            })
            .collect();
        for (more, found) in results {
            bytes = bytes.saturating_add(more);
            self.merge(found);
        }
        bytes
    }

    /// The folder's regular files and folders on the Data volume, with their metadata, and the open folder for the clone
    /// queries; `None` when it could not be read (recorded) or the walk was cancelled.
    fn list(&mut self, dir: &Path) -> Option<Listing> {
        if self.walk.cancelled() {
            return None;
        }
        let reader = match fs::read_dir(dir) {
            Ok(reader) => reader,
            Err(err) => {
                self.refuse(dir, Some(&err));
                return None;
            }
        };
        let listing = File::open(dir).ok();
        let mut entries = Vec::new();
        for entry in reader {
            if self.walk.cancelled() {
                return None;
            }
            let Ok(entry) = entry else {
                self.refuse(dir, None);
                continue;
            };
            let path = entry.path();
            let Ok(meta) = fs::symlink_metadata(&path) else {
                continue;
            };
            self.walk.tick();
            let kind = meta.file_type();
            if kind.is_file() || (kind.is_dir() && meta.dev() == self.walk.device) {
                entries.push((path, meta));
            }
        }
        Some((listing, entries))
    }

    /// The bytes `path` adds to `place` now: 0 when it is a hard link or clone settled at the end of the walk.
    fn file(&self, listing: Option<&File>, path: &Path, meta: &Metadata, place: &Place) -> u64 {
        let allocated = meta.blocks().saturating_mul(512);
        if allocated == 0 {
            return 0;
        }
        let sharing = listing
            .zip(
                path.file_name()
                    .and_then(|name| CString::new(name.as_bytes()).ok()),
            )
            .map_or(Sharing::Unknown, |(dir, name)| {
                sharing_at(dir.as_raw_fd(), &name)
            });
        let (family, bytes) = match sharing {
            Sharing::Clone { id, .. } => (Some((1, meta.dev(), id)), allocated),
            Sharing::Partial { private } => (None, private.min(allocated)),
            Sharing::Unshared | Sharing::Unknown => (None, allocated),
        };
        let key = family.or_else(|| (meta.nlink() > 1).then(|| (0, meta.dev(), meta.ino())));
        let Some(key) = key else { return bytes };
        let mut claims = self
            .walk
            .claims
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        match claims.get_mut(&key) {
            Some((held, owner)) => {
                *held = (*held).max(bytes);
                if place < owner {
                    *owner = place.clone();
                }
            }
            None => {
                claims.insert(key, (bytes, place.clone()));
            }
        }
        0
    }

    /// Records a folder the walk could not read, under the path the breakdown names and why (WF7).
    fn refuse(&mut self, dir: &Path, err: Option<&io::Error>) {
        if err.is_some_and(|err| err.kind() == io::ErrorKind::NotFound) {
            return;
        }
        let rel = self.walk.rel(dir);
        let layout = self.walk.layout;
        let other_account = rel.first().map(String::as_str) == Some("Users")
            && rel.len() >= 2
            && rel[1] != "Shared"
            && !rel.starts_with(&layout.home);
        let (why, shown) = if other_account {
            (Refusal::OtherAccount, layout.display(&rel[..2]))
        } else {
            // macOS's privacy protection guards the user's data; outside the home an `EPERM` is System Integrity Protection,
            // which Full Disk Access does not open.
            let in_home = rel.len() > layout.home.len() && rel.starts_with(&layout.home);
            let refused = err.and_then(io::Error::raw_os_error) == Some(libc::EPERM);
            let why = if refused && in_home {
                Refusal::Privacy
            } else {
                Refusal::System
            };
            (why, shown_refusal(layout, &rel))
        };
        *self.found.refused.entry((why, shown)).or_insert(0) += 1;
    }
}

/// The path a refusal is shown under: an app container's parent (`~/Library/Containers`, not each container), else the
/// folder itself.
fn shown_refusal(layout: &Layout, rel: &[String]) -> String {
    match layout.classify(rel, true) {
        Class::Item(place) if place.kind == super::ItemKind::Container => {
            layout.display(&place.rel[..place.rel.len() - 1])
        }
        _ => layout.display(rel),
    }
}

fn name_of(path: &Path) -> String {
    path.file_name().map_or_else(String::new, |name: &OsStr| {
        name.to_string_lossy().into_owned()
    })
}
