use std::collections::HashSet;
use std::path::PathBuf;

use anyhow::Result;

use crate::cli::CleanOpts;
use crate::util::format_bytes;

mod cli_validate;
mod discover;
mod execute;
mod picker;
mod picker_drill;
pub mod providers;
mod report;
mod types;

use discover::CategoryGroup;
use picker::ActionChoice;
use types::CleanAction;

pub fn run(opts: crate::cli::CleanOpts) -> Result<()> {
    cli_validate::validate(&opts)?;

    let discovery = discover::discover(&opts)?;

    println!("Cleanup candidates");
    println!();
    report::print_summary(&discovery);

    if discovery.groups.is_empty() {
        return Ok(());
    }

    if opts.dry_run {
        let refs: Vec<&CategoryGroup> = discovery.groups.iter().collect();
        report::print_plan(&refs);
        println!();
        println!("(dry-run — no changes made)");
        return Ok(());
    }

    let selected: Vec<&CategoryGroup> = if opts.yes {
        // -y + --category: skip picker entirely, plan = the named categories.
        discovery.groups.iter().collect()
    } else {
        let indices = picker::pick_categories_grouped(&discovery.groups)?;
        if indices.is_empty() {
            println!("No categories selected.");
            return Ok(());
        }
        indices.into_iter().map(|i| &discovery.groups[i]).collect()
    };

    report::print_plan(&selected);

    // Stage 2: optional drill-down. Triggered by --review-paths upfront,
    // or by the `Review paths` action menu entry mid-flow.
    let mut excluded_paths: HashSet<PathBuf> = HashSet::new();
    let mut already_drilled = false;
    if opts.review_paths && !opts.yes {
        excluded_paths = picker::drill_down(&selected)?;
        already_drilled = true;
        if !excluded_paths.is_empty() {
            println!();
            println!("({} path(s) excluded by review)", excluded_paths.len());
        }
    }

    let action = decide_action(&opts, &selected, &mut excluded_paths, &mut already_drilled)?;
    match action {
        CleanAction::DryRun => {
            println!();
            println!("(dry-run — no changes made)");
            Ok(())
        }
        CleanAction::Cancel => {
            println!("Aborted.");
            Ok(())
        }
        CleanAction::Trash | CleanAction::HardDelete => {
            if matches!(action, CleanAction::Trash)
                && selected.iter().any(|group| {
                    matches!(
                        group.id.as_str(),
                        "docker" | "docker-volumes" | "time-machine-local" | "simulator-devices"
                    )
                })
            {
                eprintln!("Warning: Docker prune, Time Machine snapshot removal and simulator device deletion cannot be undone; those tools have no Trash semantics.");
            }
            let report = execute::execute(&selected, action, &excluded_paths, &opts)?;
            print_exec_report(&report);
            Ok(())
        }
    }
}

fn decide_action(
    opts: &CleanOpts,
    selected: &[&CategoryGroup],
    excluded_paths: &mut HashSet<PathBuf>,
    already_drilled: &mut bool,
) -> Result<CleanAction> {
    if opts.yes {
        // Validation already enforced --hard + TINY_CONFIRM_HARD=1 if --hard.
        return Ok(if opts.hard {
            CleanAction::HardDelete
        } else {
            CleanAction::Trash
        });
    }
    let prefer_hard = opts.hard;
    loop {
        let offer_review = !*already_drilled;
        let choice = picker::pick_action_with_review(prefer_hard, offer_review)?;
        let action = match choice {
            ActionChoice::Trash => CleanAction::Trash,
            ActionChoice::HardDelete => CleanAction::HardDelete,
            ActionChoice::DryRun => CleanAction::DryRun,
            ActionChoice::Cancel => CleanAction::Cancel,
            ActionChoice::ReviewPaths => {
                *excluded_paths = picker::drill_down(selected)?;
                *already_drilled = true;
                if !excluded_paths.is_empty() {
                    println!();
                    println!("({} path(s) excluded by review)", excluded_paths.len());
                }
                continue;
            }
        };
        if matches!(action, CleanAction::HardDelete) {
            let total: u64 = selected.iter().map(|g| g.total_size).sum();
            let count: usize = selected.iter().map(|g| g.items.len()).sum();
            if !picker::confirm_hard_delete(count, total)? {
                return Ok(CleanAction::Cancel);
            }
        }
        return Ok(action);
    }
}

fn print_exec_report(report: &types::ExecReport) {
    println!();
    let removed = report.removed_paths.len();
    let total_size: u64 = 0; // size is not retained per-path; summary by count only
    let _ = total_size;
    println!(
        "✓ removed {} path{}",
        removed,
        if removed == 1 { "" } else { "s" }
    );
    if !report.failed.is_empty() {
        println!();
        println!("Failures ({}):", report.failed.len());
        for (path, err) in &report.failed {
            println!("  ✗ {}: {}", path.display(), err);
        }
    }
    if let Some(app) = &report.skipped_running_app {
        println!();
        println!(
            "⚠ skipped because {} is running. Quit {} and rerun.",
            app, app
        );
    }
    let _ = format_bytes; // keep import warning quiet for now
}
