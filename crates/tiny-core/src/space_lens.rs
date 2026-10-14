use crate::{
    error::{Error, Result},
    progress::{report, ProgressCallback},
};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    cmp::Reverse,
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, AtomicU64, Ordering},
    time::SystemTime,
};

/// Most children a node keeps; the rest are summarized in `omitted_*`.
const MAX_CHILDREN: usize = 100;
/// Directories shallower than this (below the scan root's children) keep children; drill down by rescanning.
const RETAIN_DEPTH: usize = 1;
const MAX_DEPTH: usize = 64;
const MAX_SKIPPED: usize = 200;

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
    /// Something below could not be read or walked, so `bytes` is a lower bound.
    pub unreadable: bool,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceReport {
    pub root: SpaceNode,
    pub files_scanned: u64,
    /// First paths that could not be read; `skipped_count` is the full total.
    pub skipped_paths: Vec<String>,
    pub skipped_count: u64,
    /// Some node left children out (see `omitted_*`); totals are still complete.
    pub truncated: bool,
    /// Some size is a lower bound (see `unreadable`).
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
pub fn scan(
    root: &Path,
    cancel: Option<&AtomicBool>,
    progress: ProgressCallback<'_>,
) -> Result<SpaceReport> {
    validate_path(root)?;
    if !fs::symlink_metadata(root)?.is_dir() {
        return Err(Error::InvalidInput("Choose a directory to scan.".into()));
    }
    let counter = AtomicU64::new(0);
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
            let mut walker = Walker::new(&counter, cancel);
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
    let mut root_node = new_node(root, &fs::symlink_metadata(root)?);
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
    let truncated = top.truncated || root_node.omitted_count > 0;
    Ok(SpaceReport {
        incomplete: root_node.unreadable || top.incomplete,
        root: root_node,
        files_scanned: counter.load(Ordering::Relaxed),
        skipped_paths: top.skipped,
        skipped_count: top.skipped_count,
        truncated,
        cancelled: cancel.is_some_and(|flag| flag.load(Ordering::Acquire)),
    })
}

/// Report-level totals gathered from every branch.
#[derive(Default)]
struct Top {
    skipped: Vec<String>,
    skipped_count: u64,
    truncated: bool,
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
        self.truncated |= walker.truncated;
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

struct Walker<'a> {
    counter: &'a AtomicU64,
    cancel: Option<&'a AtomicBool>,
    skipped: Vec<String>,
    skipped_count: u64,
    truncated: bool,
    incomplete: bool,
}

impl<'a> Walker<'a> {
    fn new(counter: &'a AtomicU64, cancel: Option<&'a AtomicBool>) -> Self {
        Self {
            counter,
            cancel,
            skipped: Vec::new(),
            skipped_count: 0,
            truncated: false,
            incomplete: false,
        }
    }

    fn cancelled(&self) -> bool {
        self.cancel.is_some_and(|flag| flag.load(Ordering::Acquire))
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
        let Ok(meta) = fs::symlink_metadata(path) else {
            self.skip(path);
            return Visit::Failed;
        };
        if meta.file_type().is_symlink() || !(meta.is_file() || meta.is_dir()) {
            return Visit::Ignored;
        }
        if path.file_name().is_none() {
            return Visit::Ignored;
        }
        let mut node = new_node(path, &meta);
        if meta.is_file() {
            self.counter.fetch_add(1, Ordering::Relaxed);
            node.bytes = meta.len();
            node.allocated_bytes = allocated(&meta);
            return Visit::Node(node);
        }
        if depth >= MAX_DEPTH {
            self.mark_unreadable(&mut node, path, false);
            return Visit::Node(node);
        }
        match fs::read_dir(path) {
            Ok(entries) => self.walk_entries(&mut node, path, entries, depth),
            Err(_) => self.mark_unreadable(&mut node, path, true),
        }
        trim(&mut node);
        self.truncated |= node.omitted_count > 0;
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
                self.mark_unreadable(node, path, true);
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

    /// Records a lower-bound size; `record` also lists the path as skipped.
    fn mark_unreadable(&mut self, node: &mut SpaceNode, path: &Path, record: bool) {
        node.unreadable = true;
        self.incomplete = true;
        if record {
            self.skip(path);
        }
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
    }
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
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    fn temp() -> tempfile::TempDir {
        let root = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        tempfile::tempdir_in(root).unwrap()
    }

    fn find<'a>(node: &'a SpaceNode, name: &str) -> &'a SpaceNode {
        node.children.iter().find(|c| c.name == name).unwrap()
    }

    #[test]
    fn lens_counts_nested_files_without_following_symlinks() {
        let dir = temp();
        let outside = temp();
        fs::write(dir.path().join("a"), b"abc").unwrap();
        fs::write(outside.path().join("b"), b"secret").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        let result = scan(dir.path(), None, None).unwrap();
        assert_eq!(result.root.bytes, 3);
        assert_eq!(result.files_scanned, 1);
        assert!(!result.incomplete && !result.truncated && !result.cancelled);
        assert!(scan(&dir.path().join("link"), None, None).is_err());
        assert!(validate_path(&dir.path().join("link/b")).is_err());
    }

    #[test]
    fn omitted_children_keep_totals_and_are_summarized() {
        let dir = temp();
        let big = dir.path().join("big");
        fs::create_dir(&big).unwrap();
        // More than twice the cap, so the streaming trim runs as well as the final one.
        let count = MAX_CHILDREN * 2 + 50;
        for i in 0..count {
            fs::write(big.join(format!("f{i:03}")), vec![0u8; i + 1]).unwrap();
        }
        let result = scan(dir.path(), None, None).unwrap();
        let node = find(&result.root, "big");
        let total: u64 = (1..=count as u64).sum();
        assert_eq!(node.bytes, total);
        assert_eq!(node.children.len(), MAX_CHILDREN);
        assert_eq!(node.omitted_count, (count - MAX_CHILDREN) as u64);
        let retained: u64 = node.children.iter().map(|c| c.bytes).sum();
        assert_eq!(retained + node.omitted_bytes, node.bytes);
        assert_eq!(
            node.children[0].bytes, count as u64,
            "largest entries are kept"
        );
        assert!(result.truncated && !result.incomplete);
    }

    #[test]
    fn root_children_beyond_the_cap_are_summarized() {
        let dir = temp();
        for i in 0..MAX_CHILDREN + 5 {
            fs::write(dir.path().join(format!("f{i}")), b"x").unwrap();
        }
        let result = scan(dir.path(), None, None).unwrap();
        assert_eq!(result.root.bytes, (MAX_CHILDREN + 5) as u64);
        assert_eq!(result.root.children.len(), MAX_CHILDREN);
        assert_eq!(result.root.omitted_count, 5);
        assert_eq!(result.root.omitted_bytes, 5);
        assert!(result.truncated);
    }

    #[test]
    fn depth_limited_folder_reports_what_it_hides() {
        let dir = temp();
        let deep = dir.path().join("a/b/c");
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("file"), b"12345").unwrap();
        let result = scan(dir.path(), None, None).unwrap();
        let b = find(find(&result.root, "a"), "b");
        assert_eq!(b.bytes, 5);
        assert!(b.children.is_empty());
        assert_eq!((b.omitted_count, b.omitted_bytes), (1, 5));
        assert!(result.truncated && !result.incomplete);
        let drilled = scan(&dir.path().join("a/b"), None, None).unwrap();
        assert_eq!(find(&drilled.root, "c").bytes, 5);
    }

    struct Restore(Vec<PathBuf>);
    impl Drop for Restore {
        fn drop(&mut self) {
            for path in &self.0 {
                let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o755));
            }
        }
    }

    fn lock(path: &Path) {
        fs::create_dir(path).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o000)).unwrap();
    }

    #[test]
    fn unreadable_folder_is_a_lower_bound_not_zero() {
        let dir = temp();
        fs::write(dir.path().join("ok"), b"abc").unwrap();
        let locked = dir.path().join("locked");
        fs::create_dir(&locked).unwrap();
        fs::write(locked.join("hidden"), b"secret").unwrap();
        let _restore = Restore(vec![locked.clone()]);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
        let result = scan(dir.path(), None, None).unwrap();
        let node = find(&result.root, "locked");
        assert!(node.unreadable && result.root.unreadable && result.incomplete);
        assert_eq!(result.skipped_count, 1);
        assert_eq!(result.skipped_paths, vec![locked.display().to_string()]);
        assert_eq!(result.root.bytes, 3);
    }

    #[test]
    fn folders_deeper_than_the_walk_limit_are_marked_not_empty() {
        let dir = temp();
        let mut deep = dir.path().to_path_buf();
        for _ in 0..MAX_DEPTH + 3 {
            deep.push("d");
        }
        fs::create_dir_all(&deep).unwrap();
        fs::write(deep.join("file"), b"x").unwrap();
        let result = scan(dir.path(), None, None).unwrap();
        assert!(result.incomplete && result.root.unreadable);
        assert_eq!(
            result.skipped_count, 0,
            "not walked is not the same as skipped"
        );
        assert_eq!(result.files_scanned, 0);
    }

    #[test]
    fn a_preset_flag_scans_nothing_and_flags_the_report() {
        let dir = temp();
        let sub = dir.path().join("sub");
        fs::create_dir(&sub).unwrap();
        for i in 0..20 {
            fs::write(sub.join(format!("f{i}")), b"x").unwrap();
        }
        let full = scan(dir.path(), None, None).unwrap();
        assert_eq!(full.files_scanned, 20);
        let flag = AtomicBool::new(true);
        let stopped = scan(dir.path(), Some(&flag), None).unwrap();
        assert!(stopped.cancelled);
        assert_eq!(stopped.files_scanned, 0);
    }

    #[test]
    fn cancelling_while_scanning_stops_the_remaining_branches() {
        let dir = temp();
        for branch in 0..8 {
            let sub = dir.path().join(format!("b{branch}"));
            fs::create_dir(&sub).unwrap();
            for i in 0..5 {
                fs::write(sub.join(format!("f{i}")), b"x").unwrap();
            }
        }
        let flag = AtomicBool::new(false);
        // One thread makes the branch order sequential: the flag flips after the first branch ends.
        let cancel_after_first_branch = |progress: crate::progress::Progress| {
            if progress.completed >= 1 {
                flag.store(true, Ordering::Release);
            }
        };
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build()
            .unwrap();
        let report = pool
            .install(|| scan(dir.path(), Some(&flag), Some(&cancel_after_first_branch)))
            .unwrap();
        assert!(report.cancelled);
        assert_eq!(
            report.files_scanned, 5,
            "only the branch that was running finished"
        );
    }

    #[test]
    fn skipped_paths_are_capped_but_counted_in_full_across_branches() {
        let dir = temp();
        let many = dir.path().join("many");
        fs::create_dir(&many).unwrap();
        let mut locked = Vec::new();
        // One branch alone exceeds the cap, and the top-level ones push the merged total past it.
        for i in 0..MAX_SKIPPED + 5 {
            let path = many.join(format!("l{i}"));
            lock(&path);
            locked.push(path);
        }
        for i in 0..10 {
            let path = dir.path().join(format!("t{i}"));
            lock(&path);
            locked.push(path);
        }
        let _restore = Restore(locked);
        let report = scan(dir.path(), None, None).unwrap();
        assert_eq!(report.skipped_paths.len(), MAX_SKIPPED);
        assert_eq!(report.skipped_count, (MAX_SKIPPED + 5 + 10) as u64);
        assert!(report.incomplete);
    }
}
