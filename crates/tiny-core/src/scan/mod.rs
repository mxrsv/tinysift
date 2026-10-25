//! `tiny scan` — read-only filesystem report.
//!
//! The command walks one or more directories (defaults to
//! `~/Downloads`, `~/Desktop`, `~/Documents`) and produces:
//!
//! - a list of files at or above `--min-size-mb`
//! - a list of files older than `--older-than-days`
//! - optional per-extension breakdown (`--by-ext`)
//! - optional duplicate detection (`--duplicates`, optionally `--hash`)
//!
//! Output mode is selectable: human-readable text (default) or
//! `--json` for downstream tooling.

pub mod duplicates;
pub mod extensions;
pub mod ignore;
pub mod sort;
pub mod types;
pub mod walker;

use std::path::{Path, PathBuf};

use crate::error::{Context, Result};

use crate::options::ScanOptions as ScanOpts;

use self::ignore::IgnoreRules;
use self::types::ScanData;

const DEFAULT_TARGET_FOLDERS: [&str; 3] = ["Downloads", "Desktop", "Documents"];

pub fn scan(opts: &ScanOpts, progress: crate::progress::ProgressCallback<'_>) -> Result<ScanData> {
    let home = home_dir();
    let roots = resolve_roots(&opts.path, home.as_deref())?;
    if roots.is_empty() {
        return Err(crate::engine_error!("no scan targets resolved — pass --path or ensure HOME contains Downloads/Desktop/Documents"));
    }

    let ignore = IgnoreRules::load(&roots, home.as_deref());

    let min_size_bytes = opts.min_size_mb.saturating_mul(1024 * 1024);
    let older_than_secs = opts.older_than_days.saturating_mul(86_400);

    crate::progress::report(progress, "scan", 0, None, "Reading files");
    let all_files = walker::walk(&roots, &ignore)?;
    let large = all_files
        .iter()
        .filter(|f| f.size >= min_size_bytes)
        .cloned()
        .collect();
    let old = all_files
        .iter()
        .filter(|f| f.age_secs >= older_than_secs)
        .cloned()
        .collect();

    let data = ScanData {
        roots,
        all_files,
        large,
        old,
    };

    crate::progress::report(
        progress,
        "scan",
        data.all_files.len() as u64,
        Some(data.all_files.len() as u64),
        "Scan complete",
    );
    Ok(data)
}

fn resolve_roots(custom: &[PathBuf], home: Option<&Path>) -> Result<Vec<PathBuf>> {
    let requested = if custom.is_empty() {
        let home = home.context("could not resolve user home directory")?;
        DEFAULT_TARGET_FOLDERS
            .iter()
            .map(|folder| home.join(folder))
            .filter_map(|path| match std::fs::symlink_metadata(&path) {
                Ok(_) => Some(Ok(path)),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                Err(error) => Some(Err(crate::engine_error!(
                    "cannot inspect default scan root {}: {error}",
                    path.display()
                ))),
            })
            .collect::<Result<Vec<_>>>()?
    } else {
        custom.to_vec()
    };
    let mut roots = Vec::new();
    for path in requested {
        let absolute = if path.is_absolute() {
            path
        } else {
            std::env::current_dir()
                .context("could not resolve current directory")?
                .join(path)
        };
        // Validate the supplied path first: normalization must not erase an
        // unsafe parent traversal or turn a symlink into an accepted root.
        crate::space_lens::validate_path(&absolute)
            .with_context(|| format!("invalid scan root {}", absolute.display()))?;
        let normalized: PathBuf = absolute.components().collect();
        if !std::fs::symlink_metadata(&normalized)?.is_dir() {
            return Err(crate::engine_error!(
                "scan root is not a directory: {}",
                normalized.display()
            ));
        }
        if !roots.contains(&normalized) {
            roots.push(normalized);
        }
    }
    // Keep nested roots: an explicit root can intentionally enter a build
    // folder, or start a fresh depth budget below a shallower root.
    Ok(roots)
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn resolve_roots_uses_custom_when_provided() {
        let directory = tempfile::tempdir().unwrap();
        let path = std::fs::canonicalize(directory.path()).unwrap();
        let roots = resolve_roots(std::slice::from_ref(&path), None).unwrap();
        assert_eq!(roots, [path]);
    }

    #[test]
    fn resolve_roots_falls_back_to_defaults_only_existing() {
        // /nonexistent home guarantees none of the defaults exist
        let home = PathBuf::from("/nonexistent_home_for_test");
        let roots = resolve_roots(&[], Some(&home)).unwrap();
        assert!(roots.is_empty());
    }
}
