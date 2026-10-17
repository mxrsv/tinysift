use crate::cli::UninstallOpts;
use crate::util::format_bytes;
use anyhow::{Context, Result};
use dialoguer::{theme::ColorfulTheme, Confirm, MultiSelect, Select};
use tiny_core::uninstall::*;

pub fn run(opts: crate::cli::UninstallOpts) -> Result<()> {
    // The duplicate-bundle-id check (O5) needs the installed apps. The picker
    // lists them fully anyway; a named app needs only their bundle ids.
    // `--shallow` plans no leftovers, so it skips the listing.
    let inventory = (!opts.shallow).then(|| match opts.name {
        Some(_) => list_app_identities().ok(),
        None => list_applications().ok(),
    });
    let targets = match opts.name.clone() {
        Some(name) => vec![resolve_app_by_name(&name)?],
        None => pick_interactive(&opts, inventory.clone().flatten())?,
    };

    if targets.is_empty() {
        println!("No apps selected.");
        return Ok(());
    }

    let plans: Vec<Plan> = targets
        .iter()
        .map(|app| {
            let apps = match &inventory {
                Some(listed) => listed.as_deref(),
                None => Some(&[][..]),
            };
            build_plan(app, &(&opts).into(), apps)
        })
        .collect::<tiny_core::error::Result<_>>()?;

    print_plans(&plans, &opts);

    let blocked: Vec<&Plan> = plans.iter().filter(|p| p.blocked.is_some()).collect();
    if !blocked.is_empty() {
        anyhow::bail!(
            "{} app(s) blocked from removal — see report above. Use --force to override Homebrew warning.",
            blocked.len()
        );
    }

    let action = decide_action(&opts)?;
    match action {
        Action::DryRun => {
            println!();
            println!("(dry-run — no changes made)");
            Ok(())
        }
        Action::Cancel => {
            println!("Aborted.");
            Ok(())
        }
        Action::Trash | Action::Hard => {
            let hard = matches!(action, Action::Hard);
            let consent = if opts.yes {
                Consent::Unattended
            } else {
                Consent::Explicit
            };
            if hard && !opts.yes && !confirm_hard_delete(&plans)? {
                println!("Aborted.");
                return Ok(());
            }
            print_kept(&plans, consent);
            run_removal(&plans, hard, consent)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Action {
    Trash,
    Hard,
    DryRun,
    Cancel,
}

fn decide_action(opts: &UninstallOpts) -> Result<Action> {
    if opts.dry_run {
        return Ok(Action::DryRun);
    }
    if opts.yes {
        return Ok(if opts.hard {
            Action::Hard
        } else {
            Action::Trash
        });
    }
    let items = [
        "Move to Trash (recoverable)",
        "Dry-run (no changes)",
        "Hard delete (NOT recoverable)",
        "Cancel",
    ];
    let default_idx = if opts.hard { 2 } else { 0 };
    let idx = Select::with_theme(&ColorfulTheme::default())
        .with_prompt("What to do?")
        .items(&items)
        .default(default_idx)
        .interact()
        .context("action menu failed")?;
    Ok(match idx {
        0 => Action::Trash,
        1 => Action::DryRun,
        2 => Action::Hard,
        _ => Action::Cancel,
    })
}

fn confirm_hard_delete(plans: &[Plan]) -> Result<bool> {
    let total: u64 = plans.iter().map(|p| p.total_size()).sum();
    let prompt = format!(
        "PERMANENTLY DELETE {} ({}) — this CANNOT be undone. Are you sure?",
        plural(
            plans
                .iter()
                .map(|p| p.items_for(Consent::Explicit).count())
                .sum::<usize>(),
            "item"
        ),
        format_bytes(total)
    );
    Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt(prompt)
        .default(false)
        .interact()
        .context("hard-delete confirm prompt failed")
}

fn plural(n: usize, label: &str) -> String {
    if n == 1 {
        format!("{} {}", n, label)
    } else {
        format!("{} {}s", n, label)
    }
}

fn run_removal(plans: &[Plan], hard: bool, consent: Consent) -> Result<()> {
    let mut failures: Vec<String> = Vec::new();
    for plan in plans {
        match execute_plan(plan, hard, consent) {
            Ok(()) => println!("✓ removed {}", plan.app.name),
            Err(e) => {
                println!("✗ {}: {}", plan.app.name, e);
                failures.push(plan.app.name.clone());
            }
        }
    }
    if !failures.is_empty() {
        anyhow::bail!("failed to fully remove: {}", failures.join(", "));
    }
    Ok(())
}

fn pick_interactive(opts: &UninstallOpts, listed: Option<Vec<AppEntry>>) -> Result<Vec<AppEntry>> {
    let apps = match listed {
        Some(apps) => apps,
        None => list_applications()?,
    };
    let apps = sort_apps(apps, &opts.sort.clone().into());

    if apps.is_empty() {
        return Ok(Vec::new());
    }

    let labels: Vec<String> = apps
        .iter()
        .map(|a| {
            let last_used = match a.last_used_days {
                Some(0) => "today".to_string(),
                Some(d) => format!("{}d ago", d),
                None => "never".to_string(),
            };
            format!(
                "{:<32} {:>10}    last used {}",
                truncate(&a.name, 32),
                format_bytes(a.size),
                last_used
            )
        })
        .collect();

    let selected = MultiSelect::with_theme(&ColorfulTheme::default())
        .with_prompt("Select apps to uninstall (Space to toggle, Enter to confirm)")
        .items(&labels)
        .interact()
        .context("interactive picker failed")?;

    Ok(selected.into_iter().map(|i| apps[i].clone()).collect())
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max - 1).collect();
        out.push('…');
        out
    }
}

// ---------- Reporting & confirm ----------

fn item_label(item: &RemovalItem) -> &'static str {
    match &item.ownership {
        Ownership::AppBundle => "app",
        Ownership::Owned {
            class: LeftoverClass::Regenerable,
        } => "cache",
        Ownership::Owned {
            class: LeftoverClass::DataRisk,
        } => "data",
        Ownership::ReportOnly { .. } => "report-only",
        Ownership::Uncertain { .. } => "uncertain",
    }
}

/// Tells the user which leftovers an unattended run leaves in place.
fn print_kept(plans: &[Plan], consent: Consent) {
    if consent != Consent::Unattended {
        return;
    }
    let kept = plans
        .iter()
        .flat_map(|p| p.items.iter())
        .filter(|i| matches!(i.ownership, Ownership::Owned { .. }) && !i.ownership.allows(consent))
        .count();
    if kept > 0 {
        println!(
            "Keeping {} (data) — `-y` removes only the app and caches. Run without -y and choose an action to remove them.",
            plural(kept, "leftover")
        );
    }
}

fn print_plans(plans: &[Plan], opts: &UninstallOpts) {
    // `-y` keeps data leftovers, so its subtotal leaves them out.
    let consent = if opts.yes {
        Consent::Unattended
    } else {
        Consent::Explicit
    };
    println!(
        "Mode: {}",
        match (opts.shallow, opts.leftovers_only, opts.hard) {
            (_, _, true) => "rm -rf (NOT recoverable)",
            (true, _, _) => "shallow (only /Applications, move to Trash)",
            (_, true, _) => "leftovers only (only ~/Library, move to Trash)",
            _ => "full (move to Trash)",
        }
    );
    println!();

    let mut grand_total = 0u64;
    for plan in plans {
        println!("== {} ==", plan.app.name);
        if let Some(reason) = &plan.blocked {
            println!("  BLOCKED: {}", reason);
        }
        match plan.app.bundle_id.as_deref() {
            Some(bid) if valid_bundle_id(bid) => println!("  bundle: {}", bid),
            Some(bid) => println!(
                "  bundle: {} (not a valid identifier; leftovers not searched)",
                bid
            ),
            None => println!("  bundle: unknown (leftovers not searched)"),
        }
        if plan.items.is_empty() {
            println!("  (nothing to remove)");
        }
        for item in &plan.items {
            println!(
                "  {:<11} {:>10}  {}",
                item_label(item),
                format_bytes(item.size),
                item.path.display()
            );
            if let Ownership::ReportOnly { reason } | Ownership::Uncertain { reason } =
                &item.ownership
            {
                println!("              ↳ {}", reason);
            }
        }
        let total: u64 = plan.items_for(consent).map(|i| i.size).sum();
        println!("  → removable subtotal: {}", format_bytes(total));
        grand_total = grand_total.saturating_add(total);
        println!();
    }
    println!("Grand total: {}", format_bytes(grand_total));
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn truncate_is_safe() {
        assert_eq!(truncate("hi", 10), "hi");
        let t = truncate("supercalifragilisticexpialidocious", 10);
        assert_eq!(t.chars().count(), 10);
        assert!(t.ends_with('…'));
    }
}
