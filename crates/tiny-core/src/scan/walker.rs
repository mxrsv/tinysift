//! Filesystem walker. Collects every file once, with size + age metadata.
//! Threshold filtering is the caller's responsibility.

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

pub fn walk(roots: &[std::path::PathBuf], ignore: &IgnoreRules) -> Vec<FileEntry> {
    let now = SystemTime::now();
    let mut out = Vec::new();
    for root in roots {
        if crate::space_lens::validate_path(root).is_err()
            || !crate::clean::fs_safe::is_dir_safe(root)
        {
            continue;
        }
        scan_dir(root, ignore, &now, 0, &mut out);
    }
    out
}

fn scan_dir(
    dir: &Path,
    ignore: &IgnoreRules,
    now: &SystemTime,
    depth: usize,
    out: &mut Vec<FileEntry>,
) {
    if depth >= MAX_DEPTH {
        return;
    }
    let entries = match fs::read_dir(dir) {
        Ok(it) => it,
        Err(_) => return,
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let metadata = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };

        if metadata.is_dir() {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();
            if builtin_skip(&name_str) || ignore.skip_dir(&name_str) {
                continue;
            }
            scan_dir(&path, ignore, now, depth + 1, out);
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
        let age_secs = metadata
            .modified()
            .ok()
            .and_then(|t| now.duration_since(t).ok())
            .map(|d| d.as_secs())
            .unwrap_or(0);

        out.push(FileEntry {
            path,
            size,
            age_secs,
        });
    }
}
