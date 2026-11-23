//! The group loop of `execute`, with an observer called after each group.
//! `execute` is this loop with an observer that never stops it.

use std::collections::HashSet;
use std::ops::ControlFlow;
use std::path::PathBuf;

use crate::engine_error;
use crate::error::Result;

use super::super::discover::CategoryGroup;
use super::super::process::AppProbe;
use super::super::protection::PolicySnapshot;
use super::super::protection_store::ProtectionStore;
use super::super::providers::CleanProvider;
use super::super::types::{CleanAction, ExecReport};
use super::{execute_group, map_action, policy_failure, reviewed_items};

/// What one group did, handed to `execute_observed`'s observer right after
/// the group ran: the paths it removed, the removals it requested, the
/// paths it kept or failed with their reasons, and the app that kept it.
#[derive(Debug, Clone)]
pub struct GroupOutcome {
    pub category_id: String,
    pub report: ExecReport,
}

/// The result of `execute_observed`: every group's report merged, and the
/// categories a `Break` left unrun, in plan order. A group that did not run
/// is neither removed nor failed.
#[derive(Debug, Default, Clone)]
pub struct ObservedRun {
    pub report: ExecReport,
    pub not_run: Vec<String>,
}

/// `execute_observed` against `store`, so tests can hold its lock.
pub(super) fn execute_observed_in(
    store: &ProtectionStore,
    groups: &[&CategoryGroup],
    action: CleanAction,
    excluded_paths: &HashSet<PathBuf>,
    providers: &[Box<dyn CleanProvider>],
    probe: &dyn AppProbe,
    on_group: &mut dyn FnMut(&GroupOutcome) -> ControlFlow<()>,
) -> Result<ObservedRun> {
    let lease = store.lease().map_err(|e| {
        engine_error!(
            "cannot read the kept paths saved by the TinySift app, so nothing was cleaned: {e}"
        )
    })?;
    observed_with_policy(
        groups,
        action,
        excluded_paths,
        lease.snapshot(),
        providers,
        probe,
        &|| {
            lease
                .ensure_unchanged()
                .map_err(|e| engine_error!("kept paths changed or unavailable: {e}"))
        },
        on_group,
    )
}

#[allow(clippy::too_many_arguments)]
pub(super) fn observed_with_policy(
    groups: &[&CategoryGroup],
    action: CleanAction,
    excluded_paths: &HashSet<PathBuf>,
    policy: &PolicySnapshot,
    providers: &[Box<dyn CleanProvider>],
    probe: &dyn AppProbe,
    unchanged: &dyn Fn() -> Result<()>,
    on_group: &mut dyn FnMut(&GroupOutcome) -> ControlFlow<()>,
) -> Result<ObservedRun> {
    let mut run = ObservedRun::default();
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
    for (index, group) in groups.iter().enumerate() {
        let mut report = ExecReport::default();
        let provider = providers.iter().find(|p| p.id() == group.id);
        match check_policy() {
            Err(error) => report.merge(policy_failure(&group.items, &error)),
            Ok(()) => {
                let filtered = reviewed_items(
                    &group.items,
                    excluded_paths,
                    &excluded_ancestors,
                    policy,
                    provider.map(|provider| provider.as_ref()),
                    &mut report,
                );
                if !filtered.is_empty() {
                    let provider =
                        provider.ok_or_else(|| engine_error!("no provider for {}", group.id))?;
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
            }
        }
        let outcome = GroupOutcome {
            category_id: group.id.clone(),
            report,
        };
        let flow = on_group(&outcome);
        run.report.merge(outcome.report);
        if flow.is_break() {
            run.not_run = groups[index + 1..].iter().map(|g| g.id.clone()).collect();
            break;
        }
    }
    Ok(run)
}

#[cfg(test)]
mod tests;
