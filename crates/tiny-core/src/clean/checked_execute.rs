//! The desktop's cleanup: each planned path is revalidated right before it
//! is moved to the Trash, deleted for good (as administrator for root-owned
//! caches), handed to its category's own tool
//! (`CleanProvider::run_desktop_tool`) or, for the Trash row, emptied, as its
//! plan says (`Removal`), and every path gets its own outcome. No failure is
//! retried another way. It never calls `CleanProvider::execute`.
//!
//! The run holds the [`StoreLease`] of the saved "always keep" rules. Before
//! each move it re-reads them: a policy that changed or cannot be read stops
//! the whole run, a path the rules protect (or whose protection cannot be
//! decided) is skipped. Neither is ever retried.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use super::elevated_delete::{ElevatedDelete, ElevatedError};
use super::failure_cause::{
    cause_of_io, skip_failure, trash_failure, Failure, FailureCause, INSIDE_SYSTEM,
};
use super::finder_trash::{Trash, TrashError};
use super::fs_safe::{Delete, PathFingerprint, TreeRemoval};
use super::process::AppProbe;
use super::protection::{Evaluation, Matcher, Relation, INTERNAL_ID_PREFIX};
use super::protection_store::{StoreError, StoreLease};
use super::providers::{desktop_report_only, CleanProvider, DesktopTool};
use super::trash_plan::{
    inside_system, protected_reason, removal_for, DeleteMode, PlannedItem, Removal,
};

mod measure;
mod report;

use measure::{frees_space, measure, settle, Measured, Settled};
pub use report::{
    CheckedExecReport, ItemOutcome, ItemResult, RemovalExtent, SkipReason, StopReason,
};

/// Everything execution needs besides the plan.
pub struct ExecContext<'a> {
    pub providers: &'a [Box<dyn CleanProvider>],
    /// The home folder (`trash_plan::trusted_home` in the app).
    pub home: Option<&'a Path>,
    pub probe: &'a dyn AppProbe,
    pub trash: &'a dyn Trash,
    /// Permanent deletion, for items planned as `Removal::Delete`.
    pub delete: &'a dyn Delete,
    /// Deletion as administrator, for items planned as
    /// `Removal::DeleteAsAdministrator`: one call per run, one dialog.
    pub elevated: &'a dyn ElevatedDelete,
    pub cancel: Option<&'a AtomicBool>,
    pub progress: crate::progress::ProgressCallback<'a>,
    /// The saved rules, locked for the whole run. There is no way to run
    /// without one: a store that holds nothing is still a store.
    pub policy: &'a StoreLease<'a>,
}

impl ExecContext<'_> {
    fn is_cancelled(&self) -> bool {
        self.cancel.is_some_and(|flag| flag.load(Ordering::Acquire))
    }

    fn report_step(&self, step: usize, total: u64, path: &Path) {
        let path = path.to_string_lossy();
        crate::progress::report(self.progress, "trash", step as u64, Some(total), &path);
    }
}

/// Removes each planned path as planned after `validate` passes for it.
/// Deletions as administrator run first, as one batch behind one
/// authentication dialog, so cancelling it changes nothing. Other permanent
/// removals come next, so emptying the Trash never takes what this run
/// moves there; if that emptying fails, Finder may still be at it, so the
/// moves are skipped. A permanent deletion goes on past entries that fail.
/// Each removal that frees space is measured right before and right after
/// it (`ItemResult::bytes_freed`). Results stay in plan order. Stops on
/// cancellation (of the run or the dialog), an Automation denial or a
/// change in the saved rules; the remaining paths are reported as not
/// attempted.
pub fn execute_checked(plan: &[PlannedItem], ctx: &ExecContext<'_>) -> CheckedExecReport {
    let total = plan.len() as u64;
    let mut stopped = None;
    let unchanged = Settled {
        extent: RemovalExtent::NotRemoved,
        freed: 0,
        left: 0,
        upper_bound: false,
    };
    let mut done = vec![(ItemOutcome::NotAttempted, unchanged, None); plan.len()];
    let (as_admin, others): (Vec<usize>, Vec<usize>) = execution_order(plan)
        .into_iter()
        .partition(|&index| plan[index].removal == Removal::DeleteAsAdministrator);
    delete_as_administrator(plan, &as_admin, ctx, &mut done, &mut stopped);
    // Finder may still be emptying after a failed or timed-out Empty Trash.
    let mut emptying_failed = false;
    for (step, index) in others.into_iter().enumerate() {
        if stopped.is_some() {
            break;
        }
        let planned = &plan[index];
        ctx.report_step(as_admin.len() + step, total, &planned.item.path);
        if emptying_failed && planned.removal == Removal::MoveToTrash {
            let reason = SkipReason::SafetyCheckFailed(
                "the Trash was not emptied cleanly, so nothing is moved into it".into(),
            );
            done[index].0 = ItemOutcome::Skipped(reason);
            continue;
        }
        let acted = act_on(planned, ctx, &mut stopped);
        emptying_failed |= planned.removal == Removal::EmptyTrash
            && matches!(acted.outcome, ItemOutcome::Failed { .. });
        done[index] = acted.settle(&planned.item.path);
    }
    if stopped.is_none() {
        crate::progress::report(ctx.progress, "trash", total, Some(total), "Done");
    }
    let results: Vec<ItemResult> = plan
        .iter()
        .zip(done)
        .map(|(planned, (outcome, settled, failure))| ItemResult {
            item: planned.item.clone(),
            failure: failure.or_else(|| failure_of(&outcome)),
            outcome,
            extent: settled.extent,
            bytes_freed: settled.freed,
            bytes_left: settled.left,
            freed_is_upper_bound: settled.upper_bound,
        })
        .collect();
    let bytes_freed = results
        .iter()
        .fold(0u64, |sum, r| sum.saturating_add(r.bytes_freed));
    let freed_is_upper_bound = results
        .iter()
        .any(|r| r.bytes_freed > 0 && r.freed_is_upper_bound);
    CheckedExecReport {
        results,
        stopped,
        bytes_freed,
        freed_is_upper_bound,
    }
}

/// The failure an outcome names by itself; a deletion's comes from its
/// route (`Acted::failure`), with this as the fallback.
fn failure_of(outcome: &ItemOutcome) -> Option<Failure> {
    match outcome {
        ItemOutcome::Skipped(reason) => skip_failure(reason),
        ItemOutcome::Failed { error, .. } => Some(trash_failure(error)),
        ItemOutcome::DeleteFailed { detail, .. } | ItemOutcome::ToolFailed { detail } => {
            Some(Failure::new(FailureCause::Other, detail.clone()))
        }
        _ => None,
    }
}

/// One item's outcome, measurement and failure, as the run reports it.
type Done = (ItemOutcome, Settled, Option<Failure>);

/// Plan indexes in run order: deletions as administrator, other deletions,
/// tool commands, then emptying the Trash, then moves to the Trash, each in
/// plan order.
fn execution_order(plan: &[PlannedItem]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..plan.len()).collect();
    order.sort_by_key(|&index| match plan[index].removal {
        Removal::DeleteAsAdministrator => 0,
        Removal::Delete => 1,
        Removal::RunTool(_) => 2,
        Removal::EmptyTrash => 3,
        Removal::MoveToTrash => 4,
    });
    order
}

/// Deletes the items planned as `DeleteAsAdministrator` (`indexes`), before
/// anything else changes. Each passes every check `act_on` runs and is
/// measured; then one call to `ctx.elevated`, one dialog, takes all that
/// passed, and each outcome is what `symlink_metadata` finds afterwards. A
/// cancelled dialog stops the run with every item not attempted. Nothing is
/// retried another way.
fn delete_as_administrator(
    plan: &[PlannedItem],
    indexes: &[usize],
    ctx: &ExecContext<'_>,
    done: &mut [Done],
    stopped: &mut Option<StopReason>,
) {
    let mut batch = Vec::new();
    for (step, &index) in indexes.iter().enumerate() {
        ctx.report_step(step, plan.len() as u64, &plan[index].item.path);
        match prepare(&plan[index], ctx) {
            Ok(before) => batch.push((index, before)),
            Err(Halt::Skip(reason)) => done[index].0 = ItemOutcome::Skipped(reason),
            Err(Halt::Stop(reason)) => {
                *stopped = Some(reason);
                return;
            }
        }
    }
    if batch.is_empty() {
        return;
    }
    // The checks of later items take time; the dialog comes right after.
    let ready = match ctx.is_cancelled() {
        true => Err(StopReason::Cancelled),
        false => policy_unchanged(ctx.policy),
    };
    if let Err(reason) = ready {
        *stopped = Some(reason);
        return;
    }
    let paths: Vec<&Path> = batch
        .iter()
        .map(|&(i, _)| plan[i].item.path.as_path())
        .collect();
    let detail = match ctx.elevated.delete(&paths) {
        Err(ElevatedError::Cancelled) => {
            *stopped = Some(StopReason::AuthenticationCancelled);
            for entry in done.iter_mut() {
                entry.0 = ItemOutcome::NotAttempted;
            }
            return;
        }
        Err(ElevatedError::Busy(app)) => {
            for &(index, _) in &batch {
                done[index].0 = ItemOutcome::Skipped(SkipReason::AppRunning(app.clone()));
            }
            return;
        }
        Err(ElevatedError::Failed(detail)) => detail,
        Ok(()) => "still there after deletion as administrator".into(),
    };
    for (index, before) in batch {
        let path = &plan[index].item.path;
        let outcome = match fs::symlink_metadata(path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => ItemOutcome::Deleted,
            _ => ItemOutcome::DeleteFailed {
                detail: detail.clone(),
                source_present: true,
            },
        };
        let failure = matches!(outcome, ItemOutcome::DeleteFailed { .. })
            .then(|| Failure::new(FailureCause::NeedsAdministrator, detail.clone()));
        let acted = Acted {
            outcome,
            before,
            removed_some: false,
            failure,
        };
        done[index] = acted.settle(path);
    }
}

/// Why an item is left as it is: skip it and go on, or end the run.
enum Halt {
    Skip(SkipReason),
    Stop(StopReason),
}

/// Every check right before `planned` changes. Returns the item's freed
/// bytes measured now when its removal frees space.
fn prepare(planned: &PlannedItem, ctx: &ExecContext<'_>) -> Result<Option<Measured>, Halt> {
    if ctx.is_cancelled() {
        return Err(Halt::Stop(StopReason::Cancelled));
    }
    // Before the slow checks, so a changed policy never spends a probe.
    policy_unchanged(ctx.policy).map_err(Halt::Stop)?;
    validate(planned, ctx).map_err(Halt::Skip)?;
    // Measured among the slow checks, so the checks below also cover the
    // time its walk takes.
    let before = frees_space(planned.removal).then(|| measure(&planned.item.path, false));
    // After them, which can take long enough for the rules to change.
    policy_unchanged(ctx.policy).map_err(Halt::Stop)?;
    if names_a_file(planned) {
        protection_skip(ctx.policy, &planned.item.path).map_err(Halt::Skip)?;
    }
    // Last check before the mutation.
    if ctx.is_cancelled() {
        return Err(Halt::Stop(StopReason::Cancelled));
    }
    Ok(before)
}

/// What acting on one item did, before it is measured again.
struct Acted {
    outcome: ItemOutcome,
    /// The measurement `prepare` took, for removals that free space.
    before: Option<Measured>,
    /// The route removed at least one entry.
    removed_some: bool,
    /// What the route knows of its failure beyond the outcome.
    failure: Option<Failure>,
}

impl Acted {
    fn untouched(outcome: ItemOutcome) -> Self {
        Self {
            outcome,
            before: None,
            removed_some: false,
            failure: None,
        }
    }

    /// Measures `path` again, right after the action, and puts a prune's
    /// measured bytes into its outcome.
    fn settle(self, path: &Path) -> Done {
        let settled = settle(path, &self.outcome, self.before, self.removed_some);
        let outcome = match self.outcome {
            ItemOutcome::Pruned { .. } => ItemOutcome::Pruned {
                bytes_freed: settled.freed,
                upper_bound: settled.upper_bound,
            },
            other => other,
        };
        (outcome, settled, self.failure)
    }
}

fn act_on(planned: &PlannedItem, ctx: &ExecContext<'_>, stopped: &mut Option<StopReason>) -> Acted {
    let before = match prepare(planned, ctx) {
        Ok(before) => before,
        Err(Halt::Skip(reason)) => return Acted::untouched(ItemOutcome::Skipped(reason)),
        Err(Halt::Stop(reason)) => {
            *stopped = Some(reason);
            return Acted::untouched(ItemOutcome::NotAttempted);
        }
    };
    let path = &planned.item.path;
    let acted = |outcome| Acted {
        outcome,
        before,
        removed_some: false,
        failure: None,
    };
    // One route per item: a failure is reported, never retried another way.
    let finder = match planned.removal {
        Removal::MoveToTrash => ctx
            .trash
            .move_to_trash(path)
            .map(|()| ItemOutcome::MovedToTrash),
        Removal::EmptyTrash => ctx.trash.empty_trash().map(|()| ItemOutcome::TrashEmptied),
        Removal::Delete => {
            let removal = ctx.delete.delete_each(path);
            let outcome = deletion_outcome(&removal, path);
            let failure = match (&outcome, removal.failures.first()) {
                (ItemOutcome::DeleteFailed { detail, .. }, Some(first)) => Some(Failure::new(
                    cause_of_io(&first.path, &first.error),
                    detail.clone(),
                )),
                _ => None,
            };
            return Acted {
                outcome,
                before,
                removed_some: removal.removed_entries > 0,
                failure,
            };
        }
        Removal::RunTool(_) => return acted(run_tool(planned, before.is_some(), ctx)),
        // Only `delete_as_administrator` acts on these, as one batch.
        Removal::DeleteAsAdministrator => return Acted::untouched(ItemOutcome::NotAttempted),
    };
    acted(finder.unwrap_or_else(|error| {
        if matches!(error, TrashError::AutomationDenied(_)) {
            *stopped = Some(StopReason::AutomationDenied);
        }
        ItemOutcome::Failed {
            error,
            source_present: fs::symlink_metadata(path).is_ok(),
        }
    }))
}

/// `Deleted` when nothing failed, otherwise `DeleteFailed` naming the first
/// entry left.
fn deletion_outcome(removal: &TreeRemoval, path: &Path) -> ItemOutcome {
    let Some(first) = removal.failures.first() else {
        return ItemOutcome::Deleted;
    };
    let more = removal.failed_entries.saturating_sub(1);
    let mut detail = match first.path == path {
        true => first.error.to_string(),
        false => format!("{}: {}", first.path.display(), first.error),
    };
    if more > 0 {
        detail.push_str(&format!(" (and {more} more left)"));
    }
    ItemOutcome::DeleteFailed {
        detail,
        source_present: fs::symlink_metadata(path).is_ok(),
    }
}

/// Runs the category's own tool for `planned`, once: a prune when its cache
/// was measured, a requested removal otherwise. The prune's bytes are filled
/// in once it is measured again; a failure is the item's outcome and nothing
/// else is tried.
fn run_tool(planned: &PlannedItem, measured: bool, ctx: &ExecContext<'_>) -> ItemOutcome {
    let item = &planned.item;
    let Some(provider) = ctx.providers.iter().find(|p| p.id() == item.category_id) else {
        return ItemOutcome::Skipped(SkipReason::UnknownCategory);
    };
    if let Err(detail) = provider.run_desktop_tool(item) {
        return ItemOutcome::ToolFailed { detail };
    }
    match measured {
        true => ItemOutcome::Pruned {
            bytes_freed: 0,
            upper_bound: false,
        },
        false => ItemOutcome::RemovalRequested,
    }
}

/// The stored rules are still exactly the ones this run holds.
pub(crate) fn policy_unchanged(policy: &StoreLease<'_>) -> Result<(), StopReason> {
    policy.ensure_unchanged().map_err(|error| match error {
        StoreError::Changed | StoreError::StoreLost => StopReason::ProtectionChanged,
        other => StopReason::ProtectionUnavailable(other.to_string()),
    })
}

/// Why the saved rules forbid moving `path`, if they do. The path alone is
/// enough: a rule inside it makes it `HoldsKept`, so nothing it would carry
/// (a covered candidate, or a kept path discovery never listed) needs its own
/// check. A fresh `Matcher` per call, since its cache describes one moment.
pub(crate) fn protection_skip(policy: &StoreLease<'_>, path: &Path) -> Result<(), SkipReason> {
    let evaluation = policy.snapshot().evaluate(&Matcher::new(), path);
    if evaluation.allows_move() {
        return Ok(());
    }
    Err(skip_for(policy, &evaluation))
}

fn skip_for(policy: &StoreLease<'_>, evaluation: &Evaluation) -> SkipReason {
    let snapshot = policy.snapshot();
    let rule_path = |id: &str| {
        snapshot
            .guarded()
            .find(|r| r.id == id)
            .map(|r| r.path.clone())
    };
    let saved = evaluation
        .blocks
        .iter()
        .filter(|b| !b.rule_id.starts_with(INTERNAL_ID_PREFIX))
        .find_map(|b| rule_path(&b.rule_id).map(|path| (path, b.relation)));
    if let Some((rule_path, relation)) = saved {
        return match relation {
            Relation::CarriesRule => SkipReason::HoldsKept { rule_path },
            Relation::Equal | Relation::InsideRule => SkipReason::Kept { rule_path },
        };
    }
    // Only Tiny's own storage rules are left: not something the user kept.
    if !evaluation.blocks.is_empty() {
        return SkipReason::ProtectedPath("Tiny's own saved settings".into());
    }
    let doubt = evaluation.uncertain.first().map(|u| u.doubt.to_string());
    SkipReason::KeepUncertain(doubt.unwrap_or_else(|| "the rules could not be applied".into()))
}

/// The per-item validator, run immediately before each removal. A path
/// inside `/System` is refused outright. The saved rules come next (cheap,
/// and the user's explicit choice); then slow checks come
/// before the path checks so those sit right before the removal: provider
/// eligibility for the planned removal, the running-app probe, the
/// providers' guards, then `symlink_metadata`, fingerprint (with the listing
/// for the Trash row), volume, root containment and protected paths. Guards
/// and app gates of every item merged into this one apply too, in every
/// removal mode.
pub fn validate(planned: &PlannedItem, ctx: &ExecContext<'_>) -> Result<(), SkipReason> {
    let item = &planned.item;
    if inside_system(&item.path) {
        return Err(SkipReason::ProtectedPath(INSIDE_SYSTEM.into()));
    }
    let on_disk = names_a_file(planned);
    if on_disk {
        protection_skip(ctx.policy, &item.path)?;
    } else if planned.removal != Removal::RunTool(DesktopTool::Request) {
        return Err(SkipReason::Missing);
    }
    let gated = gated_items(planned, ctx)?;
    let apps: BTreeSet<String> = gated
        .iter()
        .flat_map(|(provider, path)| provider.item_apps(path))
        .collect();
    for app in apps {
        match ctx.probe.probe(&app) {
            Ok(false) => {}
            Ok(true) => return Err(SkipReason::AppRunning(app)),
            Err(e) => {
                return Err(SkipReason::SafetyCheckFailed(format!(
                    "running-app check for {app} failed: {e}"
                )))
            }
        }
    }
    for (provider, path) in &gated {
        provider
            .check_item(path)
            .map_err(SkipReason::ProviderGuard)?;
    }
    if !on_disk {
        // A tool's placeholder: only its tool, after the checks above, acts on it.
        return Ok(());
    }
    let meta = fs::symlink_metadata(&item.path).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => SkipReason::Missing,
        _ => SkipReason::SafetyCheckFailed(e.to_string()),
    })?;
    if meta.file_type().is_symlink() {
        return Err(SkipReason::Symlink);
    }
    let fingerprint = match planned.removal {
        Removal::EmptyTrash => PathFingerprint::of_listing(&meta),
        _ => PathFingerprint::of(&meta),
    };
    if planned.fingerprint.as_ref() != Some(&fingerprint) {
        return Err(SkipReason::Changed);
    }
    if meta.dev() != home_device(ctx.home)? {
        return Err(SkipReason::NotOnHomeVolume);
    }
    if !planned
        .roots
        .iter()
        .any(|root| inside_root(&item.path, root))
    {
        return Err(SkipReason::OutsideRoots);
    }
    let tool_printed = gated.iter().any(|(p, _)| p.roots_from_tool_output());
    if let Some(reason) = protected_reason(&item.path, ctx.home, tool_printed) {
        return Err(SkipReason::ProtectedPath(reason));
    }
    Ok(())
}

/// False for a tool's placeholder such as `<simulator-runtime:UUID>`, a
/// relative name that every file-system check would resolve against the
/// working directory, so none may run on it.
fn names_a_file(planned: &PlannedItem) -> bool {
    planned.item.path.is_absolute()
}

/// Device of the home folder, whose volume is known to have a Trash.
pub(crate) fn home_device(home: Option<&Path>) -> Result<u64, SkipReason> {
    let home = home.ok_or_else(|| SkipReason::SafetyCheckFailed("home folder unknown".into()))?;
    fs::metadata(home)
        .map(|m| m.dev())
        .map_err(|e| SkipReason::SafetyCheckFailed(format!("cannot stat home folder: {e}")))
}

/// The item and every item merged into it, each with its provider. Any
/// unknown or report-only provider refuses the whole removal; only the Trash
/// row's own provider may empty the Trash, only a category's own tool may
/// run, and a pruned cache may also go to the Trash. A deletion needs every
/// one of them in Rebuilt automatically or Project build output
/// (`removal_for`), so a plan never deletes what only goes to the Trash, and
/// never a tool's store. Deletion as administrator needs every one of them
/// to `needs_admin`, and such a category gets no other removal.
fn gated_items<'a>(
    planned: &'a PlannedItem,
    ctx: &'a ExecContext<'_>,
) -> Result<Vec<(&'a dyn CleanProvider, &'a Path)>, SkipReason> {
    let gated = std::iter::once(&planned.item)
        .chain(&planned.covers)
        .enumerate()
        .map(|(index, item)| {
            let provider = ctx
                .providers
                .iter()
                .find(|p| p.id() == item.category_id)
                .ok_or(SkipReason::UnknownCategory)?;
            let tool = provider.desktop_tool();
            let allowed = match planned.removal {
                // Root-owned items, and all they carry, go only this way.
                Removal::DeleteAsAdministrator => provider.needs_admin(),
                _ if provider.needs_admin() => false,
                Removal::EmptyTrash if index == 0 => provider.desktop_empties_trash(),
                Removal::RunTool(planned_tool) if index == 0 => tool == Some(planned_tool),
                Removal::MoveToTrash if index == 0 && tool == Some(DesktopTool::Prune) => true,
                _ => desktop_report_only(provider.as_ref()).is_none(),
            };
            if !allowed {
                return Err(SkipReason::ReportOnly);
            }
            Ok((provider.as_ref(), item.path.as_path()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if planned.removal == Removal::Delete
        && removal_for(&planned.item, &planned.covers, DeleteMode::Permanent, None)
            != Removal::Delete
    {
        return Err(SkipReason::ProviderGuard(
            "only the Trash may take it".into(),
        ));
    }
    Ok(gated)
}

/// `path` is `root` or below it, and every parent up to the filesystem root
/// is a real directory. A discovery root is not a trusted symlink boundary:
/// tools can report the candidate itself as its root.
fn inside_root(path: &Path, root: &Path) -> bool {
    if !path.starts_with(root) {
        return false;
    }
    ancestors_are_real_dirs(path)
}

/// Every parent of `path` up to the filesystem root is a real directory, so
/// no symlink can redirect a move. Excludes `path` itself, which callers
/// check with their own `symlink_metadata`.
pub(crate) fn ancestors_are_real_dirs(path: &Path) -> bool {
    path.ancestors().skip(1).all(is_real_dir)
}

fn is_real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_dir())
}

#[cfg(test)]
mod tests;
