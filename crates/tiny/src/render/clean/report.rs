use crate::util::format_bytes;

use super::discover::{CategoryGroup, DiscoveryReport};

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
            println!("  {:>10}  {}", format_bytes(item.size), item.path.display());
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
