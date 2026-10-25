//! Filesystem walker. Collects every file once, with size + age metadata.
//! Threshold filtering is the caller's responsibility.

use crate::error::{Context, Result};
use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::time::SystemTime;

use super::ignore::IgnoreRules;
use super::types::FileEntry;

const MAX_DEPTH: usize = 12;

const BUILTIN_SKIP_DIR_NAMES: &[&str] = &[
    // VCS
    ".git",
    ".svn",
    ".hg",
    // Package / build outputs that explode in size and noise
    "node_modules",
    "vendor",
    "target",
    "build",
    "dist",
    ".next",
    ".nuxt",
    ".turbo",
    ".cache",
    "__pycache__",
    ".venv",
    "venv",
    // macOS metadata
    ".Trash",
    ".Spotlight-V100",
    ".fseventsd",
    ".DocumentRevisions-V100",
    ".TemporaryItems",
    ".DS_Store",
];

fn builtin_skip(name: &str) -> bool {
    BUILTIN_SKIP_DIR_NAMES.contains(&name)
}

pub fn walk(roots: &[std::path::PathBuf], ignore: &IgnoreRules) -> Result<Vec<FileEntry>> {
    let now = SystemTime::now();
    let mut out = Vec::new();
    for root in roots {
        crate::space_lens::validate_path(root)?;
        scan_dir(root, ignore, &now, 0, &mut out)?;
    }
    let mut seen = HashSet::new();
    out.retain(|entry| seen.insert(entry.path.clone()));
    Ok(out)
}

fn scan_dir(
    dir: &Path,
    ignore: &IgnoreRules,
    now: &SystemTime,
    depth: usize,
    out: &mut Vec<FileEntry>,
) -> Result<()> {
    if depth >= MAX_DEPTH {
        return Ok(());
    }
    let entries = fs::read_dir(dir)
        .with_context(|| format!("cannot read scan directory {}", dir.display()))?;

    for entry in entries {
        let entry = entry.with_context(|| format!("cannot read entry in {}", dir.display()))?;
        let path = entry.path();
        let metadata = fs::symlink_metadata(&path)
            .with_context(|| format!("cannot inspect scanned path {}", path.display()))?;

        if metadata.is_dir() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if builtin_skip(&name_str) || ignore.skip_dir(&name_str) {
                continue;
            }
            scan_dir(&path, ignore, now, depth + 1, out)?;
            continue;
        }
        if !metadata.is_file() {
            continue;
        }

        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if ignore.skip_file(&name_str) {
            continue;
        }

        let size = metadata.len();
        let modified = metadata
            .modified()
            .with_context(|| format!("cannot read modification time of {}", path.display()))?;
        let age_secs = now
            .duration_since(modified)
            .map(|duration| duration.as_secs())
            .unwrap_or(0);

        out.push(FileEntry {
            path,
            size,
            age_secs,
        });
    }
    Ok(())
}
