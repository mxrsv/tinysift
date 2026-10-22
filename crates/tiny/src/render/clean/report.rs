use crate::util::format_bytes;

use super::discover::{CategoryGroup, DiscoveryReport};
use super::types::{CleanItem, Evidence};

const PATH_PREVIEW_LIMIT: usize = 20;

pub fn print_summary(report: &DiscoveryReport) {
    if report.groups.is_empty() {
        println!("(no cleanup candidates discovered)");
    } else {
        for group in &report.groups {
            println!(
                "  {:<28} {:>10}   {}   ({} item{})",
                truncate(&group.label, 28),
                format_bytes(group.total_size),
                pad_badge(group.risk.badge()),
                group.items.len(),
                if group.items.len() == 1 { "" } else { "s" },
            );
        }
        let grand: u64 = report.groups.iter().map(|g| g.total_size).sum();
        println!();
        println!("  Total: {}", format_bytes(grand));
    }
    if !report.skipped_running.is_empty() {
        println!();
        for (cat, app) in &report.skipped_running {
            println!(
                "  ⚠ skipped {} — {} is running. Quit {} and rerun.",
                cat, app, app
            );
        }
    }
}

pub fn print_plan(groups: &[&CategoryGroup]) {
    println!();
    println!("== Cleanup plan ==");
    let mut grand = 0u64;
    for group in groups {
        println!();
        println!(
            "[{}] {} — {} ({} item{})",
            group.id,
            group.label,
            format_bytes(group.total_size),
            group.items.len(),
            if group.items.len() == 1 { "" } else { "s" },
        );
        for item in group.items.iter().take(PATH_PREVIEW_LIMIT) {
            let note = item_note(item)
                .map(|n| format!("  {n}"))
                .unwrap_or_default();
            println!(
                "  {:>10}  {}{note}",
                format_bytes(item.size),
                item.path.display()
            );
        }
        if group.items.len() > PATH_PREVIEW_LIMIT {
            println!("  ... and {} more", group.items.len() - PATH_PREVIEW_LIMIT);
        }
        grand = grand.saturating_add(group.total_size);
    }
    println!();
    println!("Grand total: {}", format_bytes(grand));
}

fn pad_badge(badge: &str) -> String {
    format!("{:<11}", badge)
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}

/// What an item whose path names no file is, e.g. `iOS 26.0.1 (23A8464)`
/// for a simulator runtime image.
pub(super) fn item_note(item: &CleanItem) -> Option<String> {
    item.evidence.iter().find_map(|evidence| match evidence {
        Evidence::SimulatorRuntime {
            identifier,
            version,
            build,
        } => {
            // `com.apple.CoreSimulator.SimRuntime.iOS-26-0` -> `iOS`
            let runtime = identifier.rsplit('.').next().unwrap_or(identifier);
            let platform = runtime.split('-').next().unwrap_or(runtime);
            Some(format!("{platform} {version} ({build})"))
        }
        _ => None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiny_core::clean::types::RiskLevel;

    fn item(evidence: Vec<Evidence>) -> CleanItem {
        CleanItem {
            category_id: "simulator-runtimes".into(),
            category_label: "Unused simulator runtimes".into(),
            path: "<simulator-runtime:1732E568-CE24-44BA-B87A-12934CCCC76F>".into(),
            size: 0,
            risk: RiskLevel::Review,
            evidence,
        }
    }

    #[test]
    fn simulator_runtimes_are_named_beside_their_placeholder() {
        let runtime = Evidence::SimulatorRuntime {
            identifier: "com.apple.CoreSimulator.SimRuntime.iOS-26-0".into(),
            version: "26.0.1".into(),
            build: "23A8464".into(),
        };
        assert_eq!(
            item_note(&item(vec![runtime])).as_deref(),
            Some("iOS 26.0.1 (23A8464)")
        );
        assert_eq!(item_note(&item(vec![Evidence::NotGitRepo])), None);
        assert_eq!(item_note(&item(Vec::new())), None);
    }
}
