use crate::{
    error::{Error, Result},
    progress::{report, ProgressCallback},
};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
    time::SystemTime,
};

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
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpaceReport {
    pub root: SpaceNode,
    pub files_scanned: u64,
    pub skipped_paths: Vec<String>,
    pub truncated: bool,
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

pub fn scan(root: &Path, progress: ProgressCallback<'_>) -> Result<SpaceReport> {
    validate_path(root)?;
    if !fs::symlink_metadata(root)?.is_dir() {
        return Err(Error::InvalidInput("Choose a directory to scan.".into()));
    }
    let counter = AtomicU64::new(0);
    let entries: Vec<PathBuf> = fs::read_dir(root)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .collect();
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
            let mut skipped = Vec::new();
            let mut truncated = false;
            let node = walk(path, 0, &counter, &mut skipped, &mut truncated);
            report(
                progress,
                "space-lens",
                done.fetch_add(1, Ordering::Relaxed) + 1,
                Some(entries.len() as u64),
                &path.display().to_string(),
            );
            (node, skipped, truncated)
        })
        .collect();
    let mut children = Vec::new();
    let mut skipped_paths = Vec::new();
    let mut truncated = false;
    for (node, skipped, limited) in results {
        if let Some(node) = node {
            children.push(node);
        }
        skipped_paths.extend(
            skipped
                .into_iter()
                .take(200usize.saturating_sub(skipped_paths.len())),
        );
        truncated |= limited;
    }
    children.sort_by_key(|a| std::cmp::Reverse(a.bytes));
    let bytes = children
        .iter()
        .map(|n| n.bytes)
        .fold(0u64, u64::saturating_add);
    let allocated_bytes = children
        .iter()
        .map(|n| n.allocated_bytes)
        .fold(0u64, u64::saturating_add);
    if children.len() > 200 {
        children.truncate(200);
        truncated = true;
    }
    Ok(SpaceReport {
        root: SpaceNode {
            name: root
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| root.display().to_string()),
            path: root.display().to_string(),
            bytes,
            allocated_bytes,
            age_seconds: 0,
            is_directory: true,
            children,
        },
        files_scanned: counter.load(Ordering::Relaxed),
        skipped_paths,
        truncated,
    })
}
fn walk(
    path: &Path,
    depth: usize,
    counter: &AtomicU64,
    skipped: &mut Vec<String>,
    truncated: &mut bool,
) -> Option<SpaceNode> {
    let meta = match fs::symlink_metadata(path) {
        Ok(m) => m,
        Err(_) => {
            if skipped.len() < 200 {
                skipped.push(path.display().to_string());
            }
            return None;
        }
    };
    if meta.file_type().is_symlink() {
        return None;
    }
    if !meta.is_file() && !meta.is_dir() {
        return None;
    }
    let mut node = SpaceNode {
        name: path.file_name()?.to_string_lossy().into_owned(),
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
    };
    if meta.is_file() {
        counter.fetch_add(1, Ordering::Relaxed);
        node.bytes = meta.len();
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            node.allocated_bytes = meta.blocks().saturating_mul(512);
        }
        #[cfg(not(unix))]
        {
            node.allocated_bytes = meta.len();
        }
        return Some(node);
    }
    if depth >= 64 {
        *truncated = true;
        return Some(node);
    }
    match fs::read_dir(path) {
        Ok(entries) => {
            for entry in entries {
                let entry = match entry {
                    Ok(e) => e,
                    Err(_) => {
                        if skipped.len() < 200 {
                            skipped.push(path.display().to_string());
                        }
                        continue;
                    }
                };
                if let Some(child) = walk(&entry.path(), depth + 1, counter, skipped, truncated) {
                    node.bytes = node.bytes.saturating_add(child.bytes);
                    node.allocated_bytes =
                        node.allocated_bytes.saturating_add(child.allocated_bytes);
                    // Retain directory navigation and largest file entries; sizes include omitted entries.
                    if depth < 4 {
                        node.children.push(child);
                        if node.children.len() >= 400 {
                            node.children
                                .sort_by_key(|child| std::cmp::Reverse(child.bytes));
                            node.children.truncate(200);
                            *truncated = true;
                        }
                    }
                }
            }
        }
        Err(_) => {
            if skipped.len() < 200 {
                skipped.push(path.display().to_string());
            }
        }
    }
    node.children.sort_by_key(|a| std::cmp::Reverse(a.bytes));
    if node.children.len() > 200 {
        node.children.truncate(200);
        *truncated = true;
    }
    Some(node)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn lens_counts_nested_files_without_following_symlinks() {
        let temporary_root = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let dir = tempfile::tempdir_in(&temporary_root).unwrap();
        let outside = tempfile::tempdir_in(&temporary_root).unwrap();
        fs::write(dir.path().join("a"), b"abc").unwrap();
        fs::write(outside.path().join("b"), b"secret").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        let result = scan(dir.path(), None).unwrap();
        assert_eq!(result.root.bytes, 3);
        assert_eq!(result.files_scanned, 1);
        assert!(scan(&dir.path().join("link"), None).is_err());
        assert!(validate_path(&dir.path().join("link/b")).is_err());
    }
}
