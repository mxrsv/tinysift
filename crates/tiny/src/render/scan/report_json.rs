//! JSON reporter for the scan command. Emits a single object on stdout so
//! the output can be fed to `jq` or other downstream scripts.

use serde_json::{json, Value};

use crate::cli::ScanOpts;

use super::sort::sort_files;
use super::types::{FileEntry, ScanData};

pub fn render(data: &ScanData, opts: &ScanOpts) -> anyhow::Result<()> {
    let mut large = data.large.clone();
    let mut old = data.old.clone();
    sort_files(&mut large, opts.sort.into());
    sort_files(&mut old, opts.sort.into());

    let mut payload = json!({
        "roots": data.roots.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
        "thresholds": {
            "min_size_mb": opts.min_size_mb,
            "older_than_days": opts.older_than_days,
        },
        "sort": format!("{:?}", opts.sort).to_lowercase(),
        "limit": opts.limit,
        "large_files": files_to_json(&large, opts.limit),
        "old_files": files_to_json(&old, opts.limit),
        "totals": {
            "files_scanned": data.all_files.len(),
            "large_count": large.len(),
            "old_count": old.len(),
        },
    });

    if opts.by_ext {
        let stats = super::extensions::aggregate(&data.all_files);
        let arr: Vec<Value> = stats
            .iter()
            .take(opts.limit)
            .map(|s| {
                json!({
                    "ext": s.ext,
                    "count": s.count,
                    "total_size": s.total_size,
                })
            })
            .collect();
        payload["by_extension"] = Value::Array(arr);
    }

    if opts.duplicates {
        let groups = super::duplicates::find(&data.all_files, opts.hash);
        let arr: Vec<Value> = groups
            .iter()
            .take(opts.limit)
            .map(|g| {
                json!({
                    "key": g.key,
                    "name": g.name,
                    "size": g.size,
                    "wasted_bytes": g.wasted_bytes(),
                    "verified_by_hash": g.verified_by_hash,
                    "paths": g.paths.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                })
            })
            .collect();
        payload["duplicates"] = Value::Array(arr);
        payload["duplicate_total_wasted_bytes"] =
            json!(groups.iter().map(|g| g.wasted_bytes()).sum::<u64>());
    }

    println!("{}", serde_json::to_string_pretty(&payload)?);
    Ok(())
}

fn files_to_json(files: &[FileEntry], limit: usize) -> Vec<Value> {
    files
        .iter()
        .take(limit)
        .map(|e| {
            json!({
                "path": e.path.display().to_string(),
                "size": e.size,
                "age_days": e.age_secs / 86_400,
                "age_secs": e.age_secs,
            })
        })
        .collect()
}
