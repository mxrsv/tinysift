//! A worktree's size: what moving its folder takes into the Trash, measured
//! as the blocks its files hold on disk, a hard-linked file counted once
//! within the folder (APFS clones cannot be told apart and count in full).
//! Measured on request, never per sample, and cached by the times of the
//! worktree's `HEAD` and index files: a checkout, commit or `git add` changes
//! one of them. A change git does not record (an install into an ignored
//! folder) keeps the cached size until one does.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::SystemTime;

use super::repo::pointed_git_dir;

/// Folders measured at once.
const WORKERS: usize = 4;
/// `st_blocks` counts 512-byte units.
const BLOCK: u64 = 512;

/// What a cached size was measured against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SizeKey {
    head: Option<SystemTime>,
    index: Option<SystemTime>,
}

impl SizeKey {
    /// The key of the worktree at `path` now; `None` when its `.git` does
    /// not lead to its git directory.
    pub fn of(path: &Path) -> Option<Self> {
        let admin = pointed_git_dir(path)?;
        let modified = |file: &str| {
            fs::symlink_metadata(admin.join(file))
                .and_then(|m| m.modified())
                .ok()
        };
        Some(Self {
            head: modified("HEAD"),
            index: modified("index"),
        })
    }
}

/// Sizes already measured, by worktree.
#[derive(Debug, Default)]
pub struct SizeCache {
    entries: HashMap<PathBuf, (SizeKey, u64)>,
}

impl SizeCache {
    /// The size measured for `path` under `key`.
    pub fn lookup(&self, path: &Path, key: SizeKey) -> Option<u64> {
        self.entries
            .get(path)
            .filter(|(cached, _)| *cached == key)
            .map(|(_, bytes)| *bytes)
    }

    pub fn store(&mut self, path: PathBuf, key: SizeKey, bytes: u64) {
        self.entries.insert(path, (key, bytes));
    }

    /// Forgets every worktree not in `kept`.
    pub fn retain(&mut self, kept: &[PathBuf]) {
        self.entries.retain(|path, _| kept.contains(path));
    }
}

/// [`measure`] of each of `paths`, in order, a few folders at a time.
pub fn measure_all(paths: &[PathBuf]) -> Vec<Option<u64>> {
    let next = AtomicUsize::new(0);
    let sizes: Mutex<Vec<Option<u64>>> = Mutex::new(vec![None; paths.len()]);
    std::thread::scope(|scope| {
        for _ in 0..WORKERS.min(paths.len()) {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(path) = paths.get(index) else {
                    break;
                };
                let size = measure(path);
                sizes.lock().unwrap_or_else(|e| e.into_inner())[index] = size;
            });
        }
    });
    sizes.into_inner().unwrap_or_else(|e| e.into_inner())
}

/// The blocks the folder at `path` holds, everything in it included, without
/// following a symlink or leaving its volume. `None` when it is not a folder.
pub fn measure(path: &Path) -> Option<u64> {
    let meta = fs::symlink_metadata(path).ok()?;
    if !meta.is_dir() {
        return None;
    }
    let device = meta.dev();
    let mut total = meta.blocks() * BLOCK;
    let mut linked: HashSet<u64> = HashSet::new();
    let mut folders = vec![path.to_path_buf()];
    while let Some(folder) = folders.pop() {
        let Ok(entries) = fs::read_dir(&folder) else {
            continue;
        };
        for entry in entries.flatten() {
            // `DirEntry::metadata` does not follow a symlink.
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.dev() != device {
                continue;
            }
            if meta.is_dir() {
                folders.push(entry.path());
            } else if meta.nlink() > 1 && !linked.insert(meta.ino()) {
                continue;
            }
            total += meta.blocks() * BLOCK;
        }
    }
    Some(total)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_hard_link_counts_once_and_a_symlink_is_not_followed() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("wt");
        fs::create_dir(&root).unwrap();
        fs::write(root.join("a"), vec![7u8; 64 * 1024]).unwrap();
        let alone = measure(&root).unwrap();
        fs::hard_link(root.join("a"), root.join("b")).unwrap();
        let outside = dir.path().join("outside");
        fs::create_dir(&outside).unwrap();
        fs::write(outside.join("big"), vec![1u8; 256 * 1024]).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        let with_links = measure(&root).unwrap();
        let link_blocks = fs::symlink_metadata(root.join("link")).unwrap().blocks() * BLOCK;
        assert_eq!(with_links, alone + link_blocks);
        assert!(alone >= 64 * 1024);
        assert_eq!(measure(&root.join("a")), None);
    }

    #[test]
    fn a_cached_size_holds_only_under_its_key() {
        let mut cache = SizeCache::default();
        let key = SizeKey {
            head: Some(SystemTime::UNIX_EPOCH),
            index: None,
        };
        let path = PathBuf::from("/w");
        cache.store(path.clone(), key, 10);
        assert_eq!(cache.lookup(&path, key), Some(10));
        let later = SizeKey {
            head: Some(SystemTime::now()),
            index: None,
        };
        assert_eq!(cache.lookup(&path, later), None);
        cache.retain(&[]);
        assert_eq!(cache.lookup(&path, key), None);
    }
}
