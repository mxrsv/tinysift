//! Execute a plan against per-provider executors.
//!
//! Translates the user-facing `CleanAction` into provider-specific
//! `ExecAction`s:
//! - `CleanAction::Trash` → `ExecAction::Trash` for every provider.
//!   Trash provider rejects this; we therefore refuse `Trash` whenever a
//!   `trash` group is in the plan (the user must pick Hard delete instead).
//! - `CleanAction::HardDelete` → `ExecAction::EmptyTrash` for the Trash
//!   provider, `ExecAction::HardDelete` for everyone else.

use std::collections::HashSet;
use std::path::PathBuf;

use crate::engine_error;
use crate::error::Result;

use crate::options::CleanOptions as CleanOpts;

use super::checked_execute::ancestors_are_real_dirs;
use super::discover::CategoryGroup;
use super::process::{AppProbe, RunnerProbe};
use super::protection::{Matcher, PolicySnapshot};
use super::protection_store::ProtectionStore;
use super::providers::{all_providers, CleanProvider, NEEDS_ADMIN};
use super::trash_plan::inside_system;
use super::types::{CleanAction, CleanItem, ExecAction, ExecReport};
use crate::runner::RealRunner;

const TRASH_ID: &str = "trash";

pub fn execute(
    groups: &[&CategoryGroup],
    action: CleanAction,
    excluded_paths: &HashSet<PathBuf>,
    opts: &CleanOpts,
) -> Result<ExecReport> {
    let has_trash = groups.iter().any(|g| g.id == TRASH_ID);
    if has_trash && matches!(action, CleanAction::Trash) {
        return Err(engine_error!(
            "trash category requires --hard (cannot 'move trash to trash')"
        ));
    }

    // Always keep rules are saved by the desktop app; unreadable rules stop the run.
    let store =
        ProtectionStore::for_account().map_err(|e| engine_error!("cannot open kept paths: {e}"))?;
    let lease = store.lease().map_err(|e| {
        engine_error!(
            "cannot read the kept paths saved by the TinySift app, so nothing was cleaned: {e}"
        )
    })?;
    let providers = all_providers(opts);
    execute_with_policy(
        groups,
        action,
        excluded_paths,
        lease.snapshot(),
        &providers,
        &RunnerProbe(&RealRunner),
        &|| {
            lease
                .ensure_unchanged()
                .map_err(|e| engine_error!("kept paths changed or unavailable: {e}"))
        },
    )
}

/// Same as `execute` but takes the provider list as a parameter so tests
/// can inject mocks. The public entry resolves providers via
/// `all_providers(opts)` and delegates here.
#[cfg(test)]
pub(crate) fn execute_with_providers(
    groups: &[&CategoryGroup],
    action: CleanAction,
    excluded_paths: &HashSet<PathBuf>,
    policy: &PolicySnapshot,
    providers: &[Box<dyn CleanProvider>],
    probe: &dyn AppProbe,
) -> Result<ExecReport> {
    execute_with_policy(
        groups,
        action,
        excluded_paths,
        policy,
        providers,
        probe,
        &|| Ok(()),
    )
}

fn execute_with_policy(
    groups: &[&CategoryGroup],
    action: CleanAction,
    excluded_paths: &HashSet<PathBuf>,
    policy: &PolicySnapshot,
    providers: &[Box<dyn CleanProvider>],
    probe: &dyn AppProbe,
    unchanged: &dyn Fn() -> Result<()>,
) -> Result<ExecReport> {
    let mut report = ExecReport::default();
    let stopped = std::cell::Cell::new(false);
    let check_policy = || {
        if stopped.get() {
            return Err(engine_error!(
                "kept paths changed or unavailable; run stopped"
            ));
        }
        unchanged().inspect_err(|_| stopped.set(true))
    };
    let excluded_ancestors: HashSet<_> = excluded_paths
        .iter()
        .flat_map(|path| path.ancestors())
        .collect();
    for group in groups {
        let provider = providers.iter().find(|p| p.id() == group.id);
        if let Err(error) = check_policy() {
            report.merge(policy_failure(&group.items, &error));
            continue;
        }
        let filtered = reviewed_items(
            &group.items,
            excluded_paths,
            &excluded_ancestors,
            policy,
            provider.map(|provider| provider.as_ref()),
            &mut report,
        );
        if filtered.is_empty() {
            continue;
        }
        let provider = provider.ok_or_else(|| engine_error!("no provider for {}", group.id))?;
        let exec_action = map_action(action, group.id.as_str())?;
        report.merge(execute_group(
            provider.as_ref(),
            &filtered,
            exec_action,
            probe,
            policy,
            &check_policy,
        )?);
    }
    Ok(report)
}

fn policy_failure(items: &[CleanItem], error: &crate::error::Error) -> ExecReport {
    ExecReport {
        failed: items
            .iter()
            .map(|item| (item.path.clone(), error.to_string()))
            .collect(),
        ..ExecReport::default()
    }
}

fn execute_group(
    provider: &dyn CleanProvider,
    items: &[CleanItem],
    action: ExecAction,
    probe: &dyn AppProbe,
    policy: &PolicySnapshot,
    unchanged: &dyn Fn() -> Result<()>,
) -> Result<ExecReport> {
    // The CLI never runs sudo, and Finder would ask for the password itself.
    if provider.needs_admin() {
        let reason = format!("kept: {NEEDS_ADMIN}");
        return Ok(ExecReport {
            failed: items
                .iter()
                .map(|item| (item.path.clone(), reason.clone()))
                .collect(),
            ..ExecReport::default()
        });
    }
    // Recheck file providers immediately before each path; tool-backed providers
    // retain their batch semantics (for example one Docker prune).
    if !provider.desktop_trash_paths() {
        return execute_guarded(provider, items, action, probe, policy, unchanged);
    }
    let mut report = ExecReport::default();
    for item in items {
        report.merge(execute_guarded(
            provider,
            std::slice::from_ref(item),
            action,
            probe,
            policy,
            unchanged,
        )?);
    }
    Ok(report)
}

fn reviewed_items(
    items: &[CleanItem],
    excluded: &HashSet<PathBuf>,
    ancestors: &HashSet<&std::path::Path>,
    policy: &PolicySnapshot,
    provider: Option<&dyn CleanProvider>,
    report: &mut ExecReport,
) -> Vec<CleanItem> {
    let matcher = Matcher::new();
    items
        .iter()
        .filter(|item| !excluded.contains(&item.path))
        .filter(|item| {
            let reason = if ancestors.contains(item.path.as_path())
                || item.path.ancestors().any(|path| excluded.contains(path))
            {
                "kept: overlaps a path excluded during review"
            } else {
                if provider.is_some_and(|provider| {
                    provider.owns_tool_id(&item.path) && item.category_id == provider.id()
                }) {
                    return true;
                }
                let evaluation = policy.evaluate(&matcher, &item.path);
                if evaluation.allows_move() {
                    return true;
                }
                if evaluation.blocks.is_empty() {
                    "kept: cannot confirm that no Always keep rule covers it"
                } else {
                    "kept: an Always keep rule in the TinySift app covers it"
                }
            };
            report.failed.push((item.path.clone(), reason.into()));
            false
        })
        .cloned()
        .collect()
}

fn execute_guarded(
    provider: &dyn CleanProvider,
    items: &[CleanItem],
    action: ExecAction,
    probe: &dyn AppProbe,
    policy: &PolicySnapshot,
    unchanged: &dyn Fn() -> Result<()>,
) -> Result<ExecReport> {
    if let Err(error) = unchanged() {
        return Ok(policy_failure(items, &error));
    }
    let mut report = ExecReport::default();
    let allowed: Vec<_> = items
        .iter()
        .filter(|item| {
            match check_item(provider, item, probe).and_then(|()| {
                if provider.owns_tool_id(&item.path) {
                    return Ok(());
                }
                if policy.evaluate(&Matcher::new(), &item.path).allows_move() {
                    Ok(())
                } else {
                    Err("an Always keep rule covers this path, or protection is uncertain".into())
                }
            }) {
                Ok(()) => true,
                Err(reason) => {
                    report
                        .failed
                        .push((item.path.clone(), format!("kept: {reason}")));
                    false
                }
            }
        })
        .cloned()
        .collect();
    if !allowed.is_empty() {
        if let Err(error) = unchanged() {
            report.merge(policy_failure(&allowed, &error));
            return Ok(report);
        }
        report.merge(provider.execute(&allowed, action)?);
    }
    Ok(report)
}

fn check_item(
    provider: &dyn CleanProvider,
    item: &CleanItem,
    probe: &dyn AppProbe,
) -> std::result::Result<(), String> {
    if item.category_id != provider.id() {
        return Err("item belongs to another provider".into());
    }
    if inside_system(&item.path) {
        return Err("inside /System".into());
    }
    for app in provider.item_apps(&item.path) {
        match probe.probe(&app) {
            Ok(false) => {}
            Ok(true) => return Err(format!("{app} is running")),
            Err(error) => return Err(format!("running-app check for {app} failed: {error}")),
        }
    }
    provider.check_item(&item.path)?;
    if !item.path.is_absolute() {
        if !provider.owns_tool_id(&item.path) {
            return Err("unrecognized relative tool identifier".into());
        }
    } else if !ancestors_are_real_dirs(&item.path)
        || std::fs::symlink_metadata(&item.path).is_ok_and(|m| m.file_type().is_symlink())
    {
        return Err("symlinked or unreadable path ancestors".into());
    }
    Ok(())
}

/// Map a UI action to per-provider `ExecAction`. Visible to tests.
pub fn map_action(action: CleanAction, provider_id: &str) -> Result<ExecAction> {
    match action {
        CleanAction::Trash => {
            if provider_id == TRASH_ID {
                Err(engine_error!("trash provider rejects Trash action"))
            } else {
                Ok(ExecAction::Trash)
            }
        }
        CleanAction::HardDelete => {
            if provider_id == TRASH_ID {
                Ok(ExecAction::EmptyTrash)
            } else {
                Ok(ExecAction::HardDelete)
            }
        }
        CleanAction::DryRun | CleanAction::Cancel => Err(engine_error!(
            "DryRun/Cancel must be handled before execute()"
        )),
    }
}

#[allow(dead_code)]
fn ensure_provider_exists(providers: &[Box<dyn CleanProvider>], id: &str) -> Result<()> {
    if providers.iter().any(|p| p.id() == id) {
        Ok(())
    } else {
        Err(engine_error!("no provider for '{}'", id))
    }
}

#[cfg(test)]
mod tests;
