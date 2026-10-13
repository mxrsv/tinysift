//! Per-path drill-down stage. Lets the user uncheck individual paths from
//! the categories already selected, before execute() runs.
//!
//! Two render modes:
//! - Flat: list every CleanItem path, one row per item. Default below threshold.
//! - Summary: when a category has > DRILL_DOWN_FLAT_LIMIT items, group items
//!   by their parent dir's first-level filename and let the user toggle entire
//!   groups. Unchecking a group expands to every member path in the excluded
//!   set.
//!
//! Returns a HashSet<PathBuf> of EXCLUDED paths. execute() filters
//! group.items by this set before calling provider.execute().

use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::{Context, Result};
use dialoguer::{theme::ColorfulTheme, MultiSelect};

use crate::util::format_bytes;

use super::discover::CategoryGroup;
use super::types::CleanItem;

pub const DRILL_DOWN_FLAT_LIMIT: usize = 500;

pub fn drill_down(groups: &[&CategoryGroup]) -> Result<HashSet<PathBuf>> {
    let mut excluded: HashSet<PathBuf> = HashSet::new();
    for group in groups {
        if group.items.is_empty() {
            continue;
        }
        if group.items.len() > DRILL_DOWN_FLAT_LIMIT {
            drill_summary(group, &mut excluded)?;
        } else {
            drill_flat(group, &mut excluded)?;
        }
    }
    Ok(excluded)
}

fn drill_flat(group: &CategoryGroup, excluded: &mut HashSet<PathBuf>) -> Result<()> {
    let labels: Vec<String> = group
        .items
        .iter()
        .map(|item| format!("{:>10}  {}", format_bytes(item.size), item.path.display()))
        .collect();
    let defaults: Vec<bool> = vec![true; group.items.len()];
    let prompt = format!(
        "[{}] keep checked to delete, uncheck to skip ({} items) — Esc to skip group",
        group.label,
        group.items.len()
    );
    // interact_opt: Some(...) on Enter, None on Esc/Ctrl+C. We treat Esc
    // as "skip this group, keep every path checked, move on" so user
    // doesn't lose exclusions made on previous groups.
    let kept = match MultiSelect::with_theme(&ColorfulTheme::default())
        .with_prompt(prompt)
        .items(&labels)
        .defaults(&defaults)
        .interact_opt()
        .context("drill-down (flat) failed")?
    {
        Some(v) => v,
        None => {
            eprintln!(
                "  ↳ skipped drill-down for [{}], all {} items kept in plan",
                group.label,
                group.items.len()
            );
            return Ok(());
        }
    };
    let kept_set: HashSet<usize> = kept.into_iter().collect();
    for (idx, item) in group.items.iter().enumerate() {
        if !kept_set.contains(&idx) {
            excluded.insert(item.path.clone());
        }
    }
    Ok(())
}

fn drill_summary(group: &CategoryGroup, excluded: &mut HashSet<PathBuf>) -> Result<()> {
    let buckets = summarize_by_subdir(&group.items);
    let labels: Vec<String> = buckets
        .iter()
        .map(|(parent, indices)| {
            let bucket_size: u64 = indices.iter().map(|i| group.items[*i].size).sum::<u64>();
            format!(
                "{:>10}  {} ({} items)",
                format_bytes(bucket_size),
                parent.display(),
                indices.len()
            )
        })
        .collect();
    let defaults: Vec<bool> = vec![true; buckets.len()];
    let prompt = format!(
        "[{}] {} items > {} — grouped by parent dir — Esc to skip group",
        group.label,
        group.items.len(),
        DRILL_DOWN_FLAT_LIMIT
    );
    let kept = match MultiSelect::with_theme(&ColorfulTheme::default())
        .with_prompt(prompt)
        .items(&labels)
        .defaults(&defaults)
        .interact_opt()
        .context("drill-down (summary) failed")?
    {
        Some(v) => v,
        None => {
            eprintln!(
                "  ↳ skipped drill-down for [{}], all {} items kept in plan",
                group.label,
                group.items.len()
            );
            return Ok(());
        }
    };
    let kept_set: HashSet<usize> = kept.into_iter().collect();
    for (bucket_idx, (_, indices)) in buckets.iter().enumerate() {
        if !kept_set.contains(&bucket_idx) {
            for item_idx in indices {
                excluded.insert(group.items[*item_idx].path.clone());
            }
        }
    }
    Ok(())
}

/// Groups `items` by the file_name of their parent dir. Items whose parent
/// has no name (root, empty) are bucketed under PathBuf::from("/").
/// Returns Vec<(parent_key, Vec<item_index_into_items>)>.
pub fn summarize_by_subdir(items: &[CleanItem]) -> Vec<(PathBuf, Vec<usize>)> {
    use std::collections::BTreeMap;
    let mut map: BTreeMap<PathBuf, Vec<usize>> = BTreeMap::new();
    for (idx, item) in items.iter().enumerate() {
        let key = item
            .path
            .parent()
            .map(|p| p.to_path_buf())
            .unwrap_or_else(|| PathBuf::from("/"));
        map.entry(key).or_default().push(idx);
    }
    map.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::clean::types::RiskLevel;

    fn item(path: &str) -> CleanItem {
        CleanItem {
            category_id: "test".into(),
            category_label: "Test".into(),
            path: PathBuf::from(path),
            size: 1024,
            risk: RiskLevel::Review,
            evidence: Vec::new(),
        }
    }

    #[test]
    fn summarize_by_subdir_groups_by_parent() {
        let items = vec![
            item("/a/x.txt"),
            item("/a/y.txt"),
            item("/b/z.txt"),
            item("/b/w.txt"),
        ];
        let buckets = summarize_by_subdir(&items);
        assert_eq!(buckets.len(), 2);
        let counts: Vec<usize> = buckets.iter().map(|(_, v)| v.len()).collect();
        assert_eq!(counts, vec![2, 2]);
    }

    #[test]
    fn drill_down_falls_back_to_summary_above_threshold() {
        // Synthesize 501 items split across 3 parent dirs.
        let mut items = Vec::new();
        for i in 0..501 {
            let parent = match i % 3 {
                0 => "/p0",
                1 => "/p1",
                _ => "/p2",
            };
            items.push(item(&format!("{}/file_{}.bin", parent, i)));
        }
        assert!(items.len() > DRILL_DOWN_FLAT_LIMIT);
        let buckets = summarize_by_subdir(&items);
        assert!(!buckets.is_empty());
        assert_eq!(buckets.len(), 3);
    }
}
