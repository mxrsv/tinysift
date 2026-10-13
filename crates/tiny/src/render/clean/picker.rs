use anyhow::{Context, Result};
use dialoguer::{theme::ColorfulTheme, Confirm, MultiSelect, Select};

use crate::util::format_bytes;

use super::discover::CategoryGroup;
use super::picker_drill;
use super::providers::{category_family, Family};
use super::report::truncate;
use super::types::{CleanAction, RiskLevel};

pub use picker_drill::drill_down;

/// Backwards-compatible flat picker. New code should call
/// `pick_categories_grouped`. Kept so existing tests / callers don't break.
#[allow(dead_code)]
pub fn pick_categories(groups: &[CategoryGroup]) -> Result<Vec<usize>> {
    pick_categories_grouped(groups)
}

/// Groups `groups` by family (Dev → UserStorage → System) and renders a
/// MultiSelect with disabled family-header rows + indented category rows.
/// Returns indices into the ORIGINAL `groups` slice.
pub fn pick_categories_grouped(groups: &[CategoryGroup]) -> Result<Vec<usize>> {
    if groups.is_empty() {
        return Ok(Vec::new());
    }

    let by_family = group_by_family(groups);

    // Build the flat label list dialoguer needs, plus a parallel mapping
    // back to the original group index. Header rows map to None.
    let mut labels: Vec<String> = Vec::new();
    let mut defaults: Vec<bool> = Vec::new();
    let mut origin: Vec<Option<usize>> = Vec::new();

    for (family, indices) in &by_family {
        let total: u64 = indices.iter().map(|i| groups[*i].total_size).sum();
        let max_risk = family_max_risk(groups, indices);
        labels.push(format!(
            "▸ {} — {} ({} cats) [{}]",
            family.label(),
            format_bytes(total),
            indices.len(),
            max_risk.badge()
        ));
        defaults.push(false);
        origin.push(None);

        for idx in indices {
            let g = &groups[*idx];
            labels.push(format!(
                "  • {:<26} {:>10}   {}",
                truncate(&g.label, 26),
                format_bytes(g.total_size),
                g.risk.badge()
            ));
            defaults.push(matches!(g.risk, RiskLevel::Safe));
            origin.push(Some(*idx));
        }
    }

    let selected_rows = MultiSelect::with_theme(&ColorfulTheme::default())
        .with_prompt("Select categories to clean (Space to toggle, Enter to confirm)")
        .items(&labels)
        .defaults(&defaults)
        .interact()
        .context("category picker failed")?;

    // Map back to original group indices, dropping any header rows the
    // user accidentally toggled (they have no semantic meaning).
    let mut out: Vec<usize> = selected_rows
        .into_iter()
        .filter_map(|row| origin[row])
        .collect();
    out.sort_unstable();
    out.dedup();
    Ok(out)
}

/// Group group indices by family in canonical order: Dev, UserStorage, System.
/// Empty families are omitted.
pub(crate) fn group_by_family(groups: &[CategoryGroup]) -> Vec<(Family, Vec<usize>)> {
    let order = [Family::Dev, Family::UserStorage, Family::System];
    let mut out: Vec<(Family, Vec<usize>)> = Vec::new();
    for fam in order {
        let mut indices: Vec<usize> = Vec::new();
        for (i, g) in groups.iter().enumerate() {
            if category_family(&g.id) == fam {
                indices.push(i);
            }
        }
        if !indices.is_empty() {
            out.push((fam, indices));
        }
    }
    out
}

/// Returns the most severe risk among the named indices. Destructive >
/// Review > Safe. Empty input falls back to Safe.
pub(crate) fn family_max_risk(groups: &[CategoryGroup], indices: &[usize]) -> RiskLevel {
    let mut current = RiskLevel::Safe;
    for i in indices {
        let r = groups[*i].risk;
        current = match (current, r) {
            (_, RiskLevel::Destructive) => RiskLevel::Destructive,
            (RiskLevel::Destructive, _) => RiskLevel::Destructive,
            (_, RiskLevel::Review) => RiskLevel::Review,
            (RiskLevel::Review, _) => RiskLevel::Review,
            _ => RiskLevel::Safe,
        };
    }
    current
}

/// Action menu choices for the post-plan prompt. Distinct from
/// `CleanAction` so the UI-only `ReviewPaths` choice never leaks into
/// provider code.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActionChoice {
    Trash,
    HardDelete,
    DryRun,
    ReviewPaths,
    Cancel,
}

/// Action menu shown after the plan is printed. Mirrors `tiny uninstall`.
/// Returns `CleanAction` (UI choice `ReviewPaths` not exposed by this entry —
/// see `pick_action_with_review`).
#[allow(dead_code)]
pub fn pick_action(prefer_hard: bool) -> Result<CleanAction> {
    let choice = pick_action_with_review(prefer_hard, false)?;
    Ok(match choice {
        ActionChoice::Trash => CleanAction::Trash,
        ActionChoice::HardDelete => CleanAction::HardDelete,
        ActionChoice::DryRun => CleanAction::DryRun,
        ActionChoice::Cancel => CleanAction::Cancel,
        // pick_action_with_review(_, false) never offers ReviewPaths.
        ActionChoice::ReviewPaths => unreachable!(),
    })
}

/// Action menu including the optional `Review paths` entry (drill-down).
/// `offer_review = false` hides the entry; the caller already knows whether
/// drill-down is in scope (--review-paths flag, or already drilled once).
pub fn pick_action_with_review(prefer_hard: bool, offer_review: bool) -> Result<ActionChoice> {
    // Build items dynamically; record the meaning of each row.
    let mut items: Vec<&str> = vec!["Move to Trash (recoverable)", "Dry-run (no changes)"];
    let mut mapping: Vec<ActionChoice> = vec![ActionChoice::Trash, ActionChoice::DryRun];
    if offer_review {
        items.push("Review paths (drill-down)");
        mapping.push(ActionChoice::ReviewPaths);
    }
    items.push("Hard delete (NOT recoverable)");
    mapping.push(ActionChoice::HardDelete);
    items.push("Cancel");
    mapping.push(ActionChoice::Cancel);

    let default_idx = if prefer_hard {
        mapping
            .iter()
            .position(|c| matches!(c, ActionChoice::HardDelete))
            .unwrap_or(0)
    } else {
        0
    };
    let idx = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("What to do?")
        .items(&items)
        .default(default_idx)
        .interact()
        .context("action menu failed")?;
    Ok(mapping.get(idx).copied().unwrap_or(ActionChoice::Cancel))
}

pub fn confirm_hard_delete(item_count: usize, total_size: u64) -> Result<bool> {
    let prompt = format!(
        "PERMANENTLY DELETE {} item{} ({}) — this CANNOT be undone. Are you sure?",
        item_count,
        if item_count == 1 { "" } else { "s" },
        format_bytes(total_size),
    );
    Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt(prompt)
        .default(false)
        .interact()
        .context("hard-delete confirm prompt failed")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::clean::types::{CleanItem, RiskLevel};
    use std::path::PathBuf;

    fn group(id: &str, risk: RiskLevel, size: u64) -> CategoryGroup {
        CategoryGroup {
            id: id.into(),
            label: id.into(),
            risk,
            items: vec![CleanItem {
                category_id: id.into(),
                category_label: id.into(),
                path: PathBuf::from(format!("/tmp/{}", id)),
                size,
                risk,
                evidence: Vec::new(),
            }],
            total_size: size,
        }
    }

    #[test]
    fn group_by_family_orders_dev_first() {
        let groups = vec![
            group("user-logs", RiskLevel::Safe, 100),
            group("cargo", RiskLevel::Review, 200),
            group("trash", RiskLevel::Destructive, 300),
        ];
        let buckets = group_by_family(&groups);
        assert_eq!(buckets.len(), 2, "Dev + System present, no UserStorage");
        assert_eq!(buckets[0].0, Family::Dev);
        assert_eq!(buckets[1].0, Family::System);
        assert_eq!(buckets[0].1, vec![1]);
        assert_eq!(buckets[1].1, vec![0, 2]);
    }

    #[test]
    fn group_by_family_aggregates_sizes() {
        let groups = vec![
            group("cargo", RiskLevel::Review, 100),
            group("npm", RiskLevel::Review, 200),
            group("trash", RiskLevel::Destructive, 300),
        ];
        let buckets = group_by_family(&groups);
        assert_eq!(buckets[0].0, Family::Dev);
        assert_eq!(buckets[0].1.len(), 2);
        assert_eq!(buckets[1].0, Family::System);
        assert_eq!(buckets[1].1.len(), 1);
    }

    #[test]
    fn family_max_risk_destructive_wins() {
        let groups = vec![
            group("cargo", RiskLevel::Review, 1),
            group("npm", RiskLevel::Safe, 1),
            group("trash", RiskLevel::Destructive, 1),
        ];
        assert_eq!(family_max_risk(&groups, &[0, 1, 2]), RiskLevel::Destructive);
        assert_eq!(family_max_risk(&groups, &[0, 1]), RiskLevel::Review);
        assert_eq!(family_max_risk(&groups, &[1]), RiskLevel::Safe);
    }
}
