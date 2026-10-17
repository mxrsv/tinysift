use crate::{
    error::{Error, Result},
    progress::{report, ProgressCallback},
};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    cmp::Reverse,
    collections::HashSet,
    fs, io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Mutex,
    },
    time::SystemTime,
};

/// Most children a node keeps; the rest are summarized in `omitted_*`.
const MAX_CHILDREN: usize = 100;
/// Directories shallower than this (below the scan root's children) keep children; drill down by rescanning.
const RETAIN_DEPTH: usize = 1;
const MAX_DEPTH: usize = 64;
const MAX_SKIPPED: usize = 200;
/// On macOS the data volume is also reachable through firmlinks under `/`, with the same `st_dev`.
const DATA_VOLUME: &str = "/System/Volumes/Data";

/// One file or folder. `bytes` always covers the whole subtree that could be read,
/// including children that are not retained in `children`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceNode {
    pub name: String,
    pub path: String,
    pub bytes: u64,
    pub allocated_bytes: u64,
    pub age_seconds: u64,
    pub is_directory: bool,
    pub children: Vec<SpaceNode>,
    /// Entries counted in `bytes` but left out of `children` (depth or size limit).
    pub omitted_count: u64,
    pub omitted_bytes: u64,
    /// Something at or below could not be read, so `bytes` is a lower bound.
    pub unreadable: bool,
    /// This folder, or one below it, was not walked because it is deeper than the depth limit,
    /// so `bytes` is a lower bound. Unlike `unreadable`, nothing failed.
    pub depth_limited: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceReport {
    pub root: SpaceNode,
    pub files_scanned: u64,
    /// First paths that could not be read; `skipped_count` is the full total.
    pub skipped_paths: Vec<String>,
    pub skipped_count: u64,
    /// Folders not walked because of the depth limit (see `depth_limited`).
    pub depth_limited_count: u64,
    /// Mount points on other volumes that were not entered; their bytes are not counted.
    pub other_volume_count: u64,
    /// Some size is a lower bound (see `unreadable` and `depth_limited`).
    pub incomplete: bool,
    /// Stopped early; the tree is partial and must not be presented as a result.
    pub cancelled: bool,
}

/// Reject symlinked ancestors as well as symlink roots: a safe leaf check alone is insufficient.
pub fn validate_path(path: &Path) -> Result<()> {
    if !path.is_absolute()
        || path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
    {
        return Err(Error::InvalidInput(
            "Use an absolute path without '..'.".into(),
        ));
    }
    for ancestor in path.ancestors() {
        if fs::symlink_metadata(ancestor)?.file_type().is_symlink() {
            return Err(Error::InvalidInput(format!(
                "Symlinked path is not allowed: {}",
                ancestor.display()
            )));
        }
    }
    Ok(())
}

/// Scans `root` into a depth-limited size tree. Stops early once `cancel` is set.
///
/// A regular file with several hard links counts once per scan. Other volumes are not entered
/// (see [`is_other_volume`]).
pub fn scan(
    root: &Path,
    cancel: Option<&AtomicBool>,
    progress: ProgressCallback<'_>,
) -> Result<SpaceReport> {
    scan_with_depth(root, cancel, progress, MAX_DEPTH)
}

fn scan_with_depth(
    root: &Path,
    cancel: Option<&AtomicBool>,
    progress: ProgressCallback<'_>,
    max_depth: usize,
) -> Result<SpaceReport> {
    validate_path(root)?;
    let root_meta = fs::symlink_metadata(root)?;
    if !root_meta.is_dir() {
        return Err(Error::InvalidInput("Choose a directory to scan.".into()));
    }
    let counter = AtomicU64::new(0);
    let links = Mutex::new(HashSet::new());
    let shared = Shared {
        counter: &counter,
        cancel,
        links: &links,
        root,
        root_dev: device(&root_meta),
        max_depth,
    };
    let mut top = Top::default();
    let mut entries: Vec<PathBuf> = Vec::new();
    for entry in fs::read_dir(root)? {
        match entry {
            Ok(entry) => entries.push(entry.path()),
            Err(_) => top.skip(root),
        }
    }
    report(
        progress,
        "space-lens",
        0,
        Some(entries.len() as u64),
        "Reading directory sizes",
    );
    // Parallelize immediate branches. Recursion is bounded and never follows symlinks.
    let done = AtomicU64::new(0);
    let results: Vec<_> = entries
        .par_iter()
        .map(|path| {
            let mut walker = Walker::new(shared);
            let visit = walker.walk(path, 0);
            report(
                progress,
                "space-lens",
                done.fetch_add(1, Ordering::Relaxed) + 1,
                Some(entries.len() as u64),
                &path.display().to_string(),
            );
            (visit, walker)
        })
        .collect();
    let mut root_node = new_node(root, &root_meta);
    root_node.age_seconds = 0;
    for (visit, walker) in results {
        match visit {
            Visit::Node(child) => absorb(&mut root_node, child, true),
            Visit::Failed => root_node.unreadable = true,
            Visit::Ignored => {}
        }
        top.merge(walker);
    }
    root_node.unreadable |= top.skipped_count > 0;
    trim(&mut root_node);
    Ok(SpaceReport {
        incomplete: root_node.unreadable || root_node.depth_limited || top.incomplete,
        root: root_node,
        files_scanned: counter.load(Ordering::Relaxed),
        skipped_paths: top.skipped,
        skipped_count: top.skipped_count,
        depth_limited_count: top.depth_limited_count,
        other_volume_count: top.other_volume_count,
        cancelled: cancel.is_some_and(|flag| flag.load(Ordering::Acquire)),
    })
}

/// Report-level totals gathered from every branch.
#[derive(Default)]
struct Top {
    skipped: Vec<String>,
    skipped_count: u64,
    depth_limited_count: u64,
    other_volume_count: u64,
    incomplete: bool,
}

impl Top {
    fn skip(&mut self, path: &Path) {
        self.skipped_count += 1;
        if self.skipped.len() < MAX_SKIPPED {
            self.skipped.push(path.display().to_string());
        }
    }

    fn merge(&mut self, walker: Walker<'_>) {
        self.skipped_count += walker.skipped_count;
        let room = MAX_SKIPPED.saturating_sub(self.skipped.len());
        self.skipped.extend(walker.skipped.into_iter().take(room));
        self.depth_limited_count += walker.depth_limited_count;
        self.other_volume_count += walker.other_volume_count;
        self.incomplete |= walker.incomplete;
    }
}

enum Visit {
    Node(SpaceNode),
    /// Symlink or special file: intentionally not counted.
    Ignored,
    /// Metadata could not be read: the parent's size is a lower bound.
    Failed,
}

/// Read-only state shared by every parallel branch of one scan.
#[derive(Clone, Copy)]
struct Shared<'a> {
    counter: &'a AtomicU64,
    cancel: Option<&'a AtomicBool>,
    /// `(dev, ino)` of multi-link files already counted in this scan.
    links: &'a Mutex<HashSet<(u64, u64)>>,
    root: &'a Path,
    root_dev: u64,
    max_depth: usize,
}

struct Walker<'a> {
    shared: Shared<'a>,
    skipped: Vec<String>,
    skipped_count: u64,
    depth_limited_count: u64,
    other_volume_count: u64,
    incomplete: bool,
}

impl<'a> Walker<'a> {
    fn new(shared: Shared<'a>) -> Self {
        Self {
            shared,
            skipped: Vec::new(),
            skipped_count: 0,
            depth_limited_count: 0,
            other_volume_count: 0,
            incomplete: false,
        }
    }

    fn cancelled(&self) -> bool {
        self.shared
            .cancel
            .is_some_and(|flag| flag.load(Ordering::Acquire))
    }

    fn skip(&mut self, path: &Path) {
        self.incomplete = true;
        self.skipped_count += 1;
        if self.skipped.len() < MAX_SKIPPED {
            self.skipped.push(path.display().to_string());
        }
    }

    fn walk(&mut self, path: &Path, depth: usize) -> Visit {
        if self.cancelled() {
            return Visit::Ignored;
        }
        let meta = match fs::symlink_metadata(path) {
            Ok(meta) => meta,
            Err(err) => return self.stat_failed(path, &err),
        };
        if meta.file_type().is_symlink() || !(meta.is_file() || meta.is_dir()) {
            return Visit::Ignored;
        }
        if path.file_name().is_none() {
            return Visit::Ignored;
        }
        if meta.is_file() {
            return self.walk_file(path, &meta);
        }
        if is_other_volume(self.shared.root, self.shared.root_dev, path, device(&meta)) {
            self.other_volume_count += 1;
            return Visit::Ignored;
        }
        self.walk_dir(path, &meta, depth)
    }

    /// A vanished entry is not an error; anything else makes the parent a lower bound.
    fn stat_failed(&mut self, path: &Path, err: &io::Error) -> Visit {
        if err.kind() == io::ErrorKind::NotFound {
            return Visit::Ignored;
        }
        self.skip(path);
        Visit::Failed
    }

    fn walk_file(&mut self, path: &Path, meta: &fs::Metadata) -> Visit {
        if let Some(id) = shared_link_id(meta) {
            let mut seen = self.shared.links.lock().unwrap_or_else(|e| e.into_inner());
            if !seen.insert(id) {
                return Visit::Ignored;
            }
        }
        let mut node = new_node(path, meta);
        self.shared.counter.fetch_add(1, Ordering::Relaxed);
        node.bytes = meta.len();
        node.allocated_bytes = allocated(meta);
        Visit::Node(node)
    }

    fn walk_dir(&mut self, path: &Path, meta: &fs::Metadata, depth: usize) -> Visit {
        let mut node = new_node(path, meta);
        if depth >= self.shared.max_depth {
            node.depth_limited = true;
            self.depth_limited_count += 1;
            self.incomplete = true;
            return Visit::Node(node);
        }
        match fs::read_dir(path) {
            Ok(entries) => self.walk_entries(&mut node, path, entries, depth),
            Err(_) => self.mark_unreadable(&mut node, path),
        }
        trim(&mut node);
        Visit::Node(node)
    }

    fn walk_entries(
        &mut self,
        node: &mut SpaceNode,
        path: &Path,
        entries: fs::ReadDir,
        depth: usize,
    ) {
        // No cancel check here: `walk` checks before reading each entry, which stops the loop's work.
        for entry in entries {
            let Ok(entry) = entry else {
                self.mark_unreadable(node, path);
                continue;
            };
            match self.walk(&entry.path(), depth + 1) {
                Visit::Node(child) => {
                    absorb(node, child, depth < RETAIN_DEPTH);
                    if node.children.len() >= MAX_CHILDREN * 2 {
                        trim(node);
                    }
                }
                Visit::Failed => node.unreadable = true,
                Visit::Ignored => {}
            }
        }
    }

    /// Records a lower-bound size and lists the path as skipped.
    fn mark_unreadable(&mut self, node: &mut SpaceNode, path: &Path) {
        node.unreadable = true;
        self.skip(path);
    }
}

fn new_node(path: &Path, meta: &fs::Metadata) -> SpaceNode {
    SpaceNode {
        name: path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.display().to_string()),
        path: path.display().to_string(),
        bytes: 0,
        allocated_bytes: 0,
        age_seconds: meta
            .modified()
            .ok()
            .and_then(|t| SystemTime::now().duration_since(t).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0),
        is_directory: meta.is_dir(),
        children: Vec::new(),
        omitted_count: 0,
        omitted_bytes: 0,
        unreadable: false,
        depth_limited: false,
    }
}

/// True for a directory on another volume than the scan root. The device check alone is not
/// enough for `/`: on macOS `/System/Volumes/Data` has the same device, so it is named explicitly.
fn is_other_volume(root: &Path, root_dev: u64, path: &Path, dev: u64) -> bool {
    // Any root that reaches the data volume is an ancestor of it (`/`, `/System`, ...).
    let data = Path::new(DATA_VOLUME);
    dev != root_dev || (path == data && root != data)
}

/// `(dev, ino)` of a regular file with more than one hard link; `None` when it needs no dedup.
#[cfg(unix)]
fn shared_link_id(meta: &fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    (meta.nlink() > 1).then(|| (meta.dev(), meta.ino()))
}

#[cfg(not(unix))]
fn shared_link_id(_: &fs::Metadata) -> Option<(u64, u64)> {
    None
}

#[cfg(unix)]
fn device(meta: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.dev()
}

#[cfg(not(unix))]
fn device(_: &fs::Metadata) -> u64 {
    0
}

#[cfg(unix)]
fn allocated(meta: &fs::Metadata) -> u64 {
    use std::os::unix::fs::MetadataExt;
    meta.blocks().saturating_mul(512)
}

#[cfg(not(unix))]
fn allocated(meta: &fs::Metadata) -> u64 {
    meta.len()
}

/// Adds `child` to `node`'s totals; keeps it as a child only when `retain`, else records it as omitted.
fn absorb(node: &mut SpaceNode, child: SpaceNode, retain: bool) {
    node.bytes = node.bytes.saturating_add(child.bytes);
    node.allocated_bytes = node.allocated_bytes.saturating_add(child.allocated_bytes);
    node.unreadable |= child.unreadable;
    node.depth_limited |= child.depth_limited;
    if retain {
        node.children.push(child);
    } else {
        node.omitted_count += 1;
        node.omitted_bytes = node.omitted_bytes.saturating_add(child.bytes);
    }
}

/// Keeps the largest `MAX_CHILDREN` children and moves the rest into the omitted summary.
fn trim(node: &mut SpaceNode) {
    node.children.sort_by_key(|child| Reverse(child.bytes));
    if node.children.len() > MAX_CHILDREN {
        for child in node.children.drain(MAX_CHILDREN..) {
            node.omitted_count += 1;
            node.omitted_bytes = node.omitted_bytes.saturating_add(child.bytes);
        }
    }
}

#[cfg(test)]
mod tests;
