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
    let home = home_dir().context("could not resolve user home directory")?;
    let roots = resolve_roots(&opts.path, &home);
    if roots.is_empty() {
        return Err(crate::engine_error!("no scan targets resolved — pass --path or ensure HOME contains Downloads/Desktop/Documents"));
    }

    let ignore = IgnoreRules::load(&roots, Some(&home));

    let min_size_bytes = opts.min_size_mb.saturating_mul(1024 * 1024);
    let older_than_secs = opts.older_than_days.saturating_mul(86_400);

    crate::progress::report(progress, "scan", 0, None, "Reading files");
    let all_files = walker::walk(&roots, &ignore);
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

fn resolve_roots(custom: &[PathBuf], home: &Path) -> Vec<PathBuf> {
    if !custom.is_empty() {
        return custom
            .iter()
            .map(|p| {
                if p.is_absolute() {
                    p.clone()
                } else {
                    std::env::current_dir()
                        .map(|cwd| cwd.join(p))
                        .unwrap_or_else(|_| p.clone())
                }
            })
            .collect();
    }
    DEFAULT_TARGET_FOLDERS
        .iter()
        .map(|f| home.join(f))
        .filter(|p| p.is_dir())
        .collect()
}

fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

#[cfg(test)]
pub mod tests {
    use super::*;

    #[test]
    fn resolve_roots_uses_custom_when_provided() {
        let home = PathBuf::from("/tmp/home");
        let custom = vec![PathBuf::from("/tmp/a"), PathBuf::from("/tmp/b")];
        let roots = resolve_roots(&custom, &home);
        assert_eq!(roots, custom);
    }

    #[test]
    fn resolve_roots_falls_back_to_defaults_only_existing() {
        // /nonexistent home guarantees none of the defaults exist
        let home = PathBuf::from("/nonexistent_home_for_test");
        let roots = resolve_roots(&[], &home);
        assert!(roots.is_empty());
    }
}
