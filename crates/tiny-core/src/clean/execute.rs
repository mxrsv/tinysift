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
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

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

mod observed;

use observed::execute_observed_in;
#[cfg(test)]
use observed::observed_with_policy;
pub use observed::{GroupOutcome, ObservedRun};

const TRASH_ID: &str = "trash";

/// The paths unticked during review, and the rule the executor applies to
/// them: an item is kept when it is one of them, lies inside one, or contains
/// one. Anything that shows the size of a plan must apply the same rule.
pub struct ReviewExclusion<'a> {
    excluded: &'a HashSet<PathBuf>,
    /// Every unticked path and every folder above one.
    ancestors: HashSet<&'a Path>,
}

impl<'a> ReviewExclusion<'a> {
    pub fn new(excluded: &'a HashSet<PathBuf>) -> Self {
        Self {
            excluded,
            ancestors: excluded.iter().flat_map(|path| path.ancestors()).collect(),
        }
    }

    /// The executor keeps `path`: it was unticked, or it overlaps an unticked path.
    pub fn keeps(&self, path: &Path) -> bool {
        self.ancestors.contains(path) || path.ancestors().any(|above| self.excluded.contains(above))
    }

    fn is_unticked(&self, path: &Path) -> bool {
        self.excluded.contains(path)
    }
}

pub fn execute(
    groups: &[&CategoryGroup],
    action: CleanAction,
    excluded_paths: &HashSet<PathBuf>,
    opts: &CleanOpts,
) -> Result<ExecReport> {
    execute_observed(groups, action, excluded_paths, opts, &mut |_| {
        ControlFlow::Continue(())
    })
    .map(|run| run.report)
}

/// `execute`, reporting each group to `on_group` as soon as it ran. The run
/// holds one kept-paths lease for every group; `Break` stops it before the
/// next group, which is then reported in `ObservedRun::not_run`. A group in
/// progress always finishes: nothing is stopped inside one.
pub fn execute_observed(
    groups: &[&CategoryGroup],
    action: CleanAction,
    excluded_paths: &HashSet<PathBuf>,
    opts: &CleanOpts,
    on_group: &mut dyn FnMut(&GroupOutcome) -> ControlFlow<()>,
) -> Result<ObservedRun> {
    let has_trash = groups.iter().any(|g| g.id == TRASH_ID);
    if has_trash && matches!(action, CleanAction::Trash) {
        return Err(engine_error!(
            "trash category requires --hard (cannot 'move trash to trash')"
        ));
    }

    // Always keep rules are saved by the desktop app; unreadable rules stop the run.
    let store =
        ProtectionStore::for_account().map_err(|e| engine_error!("cannot open kept paths: {e}"))?;
    let providers = all_providers(opts);
    execute_observed_in(
        &store,
        groups,
        action,
        excluded_paths,
        &providers,
        &RunnerProbe(&RealRunner),
        on_group,
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

#[cfg(test)]
fn execute_with_policy(
    groups: &[&CategoryGroup],
    action: CleanAction,
    excluded_paths: &HashSet<PathBuf>,
    policy: &PolicySnapshot,
    providers: &[Box<dyn CleanProvider>],
    probe: &dyn AppProbe,
    unchanged: &dyn Fn() -> Result<()>,
) -> Result<ExecReport> {
    observed_with_policy(
        groups,
        action,
        excluded_paths,
        policy,
        providers,
        probe,
        unchanged,
        &mut |_| ControlFlow::Continue(()),
    )
    .map(|run| run.report)
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
    review: &ReviewExclusion<'_>,
    policy: &PolicySnapshot,
    provider: Option<&dyn CleanProvider>,
    report: &mut ExecReport,
) -> Vec<CleanItem> {
    let matcher = Matcher::new();
    items
        .iter()
        .filter(|item| !review.is_unticked(&item.path))
        .filter(|item| {
            let reason = if review.keeps(&item.path) {
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

#[cfg(test)]
mod tests;
