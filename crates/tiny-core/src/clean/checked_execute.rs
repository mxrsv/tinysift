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
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use super::elevated_delete::{ElevatedDelete, ElevatedError};
use super::finder_trash::{Trash, TrashError};
use super::fs_safe::{dir_size_checked, Delete, PathFingerprint};
use super::process::AppProbe;
use super::protection::{Evaluation, Matcher, Relation, INTERNAL_ID_PREFIX};
use super::protection_store::{StoreError, StoreLease};
use super::providers::{desktop_report_only, CleanProvider, DesktopTool};
use super::scan_context::ScanContext;
use super::trash_plan::{
    freed_together, inside_system, protected_reason, removal_for, DeleteMode, PlannedItem, Removal,
};
use super::types::CleanItem;

/// Why a path was not moved even though it was planned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SkipReason {
    /// The category is report-only on the desktop.
    ReportOnly,
    /// No registered provider has the item's category ID.
    UnknownCategory,
    Missing,
    /// The path itself is a symlink now.
    Symlink,
    /// Replaced or edited since discovery.
    Changed,
    /// Not inside a discovery root, or a symlink appeared on the way to it.
    OutsideRoots,
    /// The file-system root, the home folder or one of its ancestors, a
    /// tool-reported path outside the home folder, or anything in `/System`.
    ProtectedPath(String),
    /// On another volume than the home folder. Finder may delete such a
    /// path permanently when that volume has no Trash.
    NotOnHomeVolume,
    /// The provider's own guard refused the path.
    ProviderGuard(String),
    AppRunning(String),
    /// A required safety check could not run; acting would be a guess.
    SafetyCheckFailed(String),
    /// A saved rule keeps the path itself, or it lies inside a kept folder.
    Kept {
        rule_path: PathBuf,
    },
    /// The path is a parent of a saved rule's path, so moving it would carry
    /// the kept path along, found by discovery or not.
    HoldsKept {
        rule_path: PathBuf,
    },
    /// Whether a saved rule covers the path could not be decided.
    KeepUncertain(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ItemOutcome {
    MovedToTrash,
    /// Deleted for good (`Removal::Delete`), or gone after deletion as
    /// administrator.
    Deleted,
    /// Finder emptied the Trash (`Removal::EmptyTrash`).
    TrashEmptied,
    /// Finder refused to move the path or to empty the Trash.
    /// `source_present` is re-checked afterwards.
    Failed {
        error: TrashError,
        source_present: bool,
    },
    /// Permanent deletion failed, or the path is still there after deletion
    /// as administrator; part of it may be gone. Nothing else is tried: no
    /// Trash, no other route, never administrator rights after a failure.
    DeleteFailed {
        detail: String,
        source_present: bool,
    },
    /// The tool pruned the cache folder: `bytes_freed` is its size before
    /// minus after, an upper bound when the volume could not tell which
    /// blocks other files share or the walk after could not read everything.
    Pruned {
        bytes_freed: u64,
        upper_bound: bool,
    },
    /// The tool accepted the removal and finishes it in the background
    /// (`simctl runtime delete`): neither freed nor moved yet.
    RemovalRequested,
    /// The tool command failed or could not run; part of the cache may be
    /// pruned. Nothing else is tried: no deletion, no Trash, no elevation.
    ToolFailed {
        detail: String,
    },
    Skipped(SkipReason),
    /// Execution stopped before this path.
    NotAttempted,
}

impl ItemOutcome {
    /// Deleted, pruned, or the Trash emptied: the item's bytes are free now.
    pub fn frees_space(&self) -> bool {
        matches!(
            self,
            Self::Deleted | Self::TrashEmptied | Self::Pruned { .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    Cancelled,
    /// The user cancelled the authentication dialog for deletion as
    /// administrator, which comes before anything changes.
    AuthenticationCancelled,
    AutomationDenied,
    /// The saved rules differ from what this run started with.
    ProtectionChanged,
    /// The saved rules could not be read again.
    ProtectionUnavailable(String),
}

#[derive(Debug, Clone)]
pub struct ItemResult {
    pub item: CleanItem,
    pub outcome: ItemOutcome,
}

#[derive(Debug, Clone, Default)]
pub struct CheckedExecReport {
    /// One entry per planned item, in plan order.
    pub results: Vec<ItemResult>,
    pub stopped: Option<StopReason>,
    /// What the deleted items and the emptied Trash freed, blocks shared
    /// between them counted once (`freed_together`), plus what each prune
    /// freed as measured around it.
    pub bytes_freed: u64,
    /// `bytes_freed` counts blocks the volume could not show as unshared.
    pub freed_is_upper_bound: bool,
}

impl CheckedExecReport {
    /// Sizes measured at discovery of every planned item.
    pub fn bytes_selected(&self) -> u64 {
        self.sum(|_| true)
    }

    /// Sizes measured at discovery of the items moved to Trash. Moving to
    /// Trash frees no space until Trash is emptied.
    pub fn bytes_moved_to_trash(&self) -> u64 {
        self.sum(|outcome| *outcome == ItemOutcome::MovedToTrash)
    }

    /// Sizes measured at discovery of the items whose removal a tool
    /// accepted and finishes in the background: neither freed nor moved.
    pub fn bytes_requested(&self) -> u64 {
        self.sum(|outcome| *outcome == ItemOutcome::RemovalRequested)
    }

    pub fn moved_count(&self) -> usize {
        self.count(|outcome| *outcome == ItemOutcome::MovedToTrash)
    }

    pub fn count(&self, keep: impl Fn(&ItemOutcome) -> bool) -> usize {
        self.results.iter().filter(|r| keep(&r.outcome)).count()
    }

    fn sum(&self, keep: impl Fn(&ItemOutcome) -> bool) -> u64 {
        self.results
            .iter()
            .filter(|r| keep(&r.outcome))
            .fold(0, |sum, r| sum.saturating_add(r.item.size))
    }
}

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
/// moves are skipped. Results stay in plan order. Stops on cancellation (of
/// the run or the dialog), an Automation denial or a change in the saved
/// rules; the remaining paths are reported as not attempted.
pub fn execute_checked(plan: &[PlannedItem], ctx: &ExecContext<'_>) -> CheckedExecReport {
    let total = plan.len() as u64;
    let mut stopped = None;
    let mut outcomes = vec![ItemOutcome::NotAttempted; plan.len()];
    let (as_admin, others): (Vec<usize>, Vec<usize>) = execution_order(plan)
        .into_iter()
        .partition(|&index| plan[index].removal == Removal::DeleteAsAdministrator);
    delete_as_administrator(plan, &as_admin, ctx, &mut outcomes, &mut stopped);
    // Finder may still be emptying after a failed or timed-out Empty Trash.
    let mut emptying_failed = false;
    for (step, index) in others.into_iter().enumerate() {
        if stopped.is_some() {
            break;
        }
        let planned = &plan[index];
        ctx.report_step(as_admin.len() + step, total, &planned.item.path);
        if emptying_failed && planned.removal == Removal::MoveToTrash {
            outcomes[index] = ItemOutcome::Skipped(SkipReason::SafetyCheckFailed(
                "the Trash was not emptied cleanly, so nothing is moved into it".into(),
            ));
            continue;
        }
        outcomes[index] = act_on(planned, ctx, &mut stopped);
        emptying_failed |= planned.removal == Removal::EmptyTrash
            && matches!(outcomes[index], ItemOutcome::Failed { .. });
    }
    if stopped.is_none() {
        crate::progress::report(ctx.progress, "trash", total, Some(total), "Done");
    }
    // A prune's freed bytes were measured around it, so it stays out of the union.
    let (removed, removed_upper_bound) = freed_together(
        plan.iter()
            .zip(&outcomes)
            .filter(|(_, outcome)| {
                matches!(outcome, ItemOutcome::Deleted | ItemOutcome::TrashEmptied)
            })
            .map(|(planned, _)| planned),
    );
    let (pruned, pruned_upper_bound) =
        outcomes
            .iter()
            .fold((0u64, false), |(sum, upper), outcome| match outcome {
                ItemOutcome::Pruned {
                    bytes_freed,
                    upper_bound,
                } => (sum.saturating_add(*bytes_freed), upper || *upper_bound),
                _ => (sum, upper),
            });
    let bytes_freed = removed.saturating_add(pruned);
    let freed_is_upper_bound = removed_upper_bound || pruned_upper_bound;
    let results = plan
        .iter()
        .zip(outcomes)
        .map(|(planned, outcome)| ItemResult {
            item: planned.item.clone(),
            outcome,
        })
        .collect();
    CheckedExecReport {
        results,
        stopped,
        bytes_freed,
        freed_is_upper_bound,
    }
}

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
/// anything else changes. Each passes every check `act_on` runs; then one
/// call to `ctx.elevated`, one dialog, takes all that passed, and each
/// outcome is what `symlink_metadata` finds afterwards. A cancelled dialog
/// stops the run with every item not attempted. Nothing is retried another
/// way.
fn delete_as_administrator(
    plan: &[PlannedItem],
    indexes: &[usize],
    ctx: &ExecContext<'_>,
    outcomes: &mut [ItemOutcome],
    stopped: &mut Option<StopReason>,
) {
    let mut batch = Vec::new();
    for (step, &index) in indexes.iter().enumerate() {
        ctx.report_step(step, plan.len() as u64, &plan[index].item.path);
        match prepare(&plan[index], ctx) {
            Ok(_) => batch.push(index),
            Err(Halt::Skip(reason)) => outcomes[index] = ItemOutcome::Skipped(reason),
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
    let paths: Vec<&Path> = batch.iter().map(|&i| plan[i].item.path.as_path()).collect();
    let detail = match ctx.elevated.delete(&paths) {
        Err(ElevatedError::Cancelled) => {
            *stopped = Some(StopReason::AuthenticationCancelled);
            outcomes.fill(ItemOutcome::NotAttempted);
            return;
        }
        Err(ElevatedError::Busy(app)) => {
            for &index in &batch {
                outcomes[index] = ItemOutcome::Skipped(SkipReason::AppRunning(app.clone()));
            }
            return;
        }
        Err(ElevatedError::Failed(detail)) => detail,
        Ok(()) => "still there after deletion as administrator".into(),
    };
    for &index in &batch {
        outcomes[index] = match fs::symlink_metadata(&plan[index].item.path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => ItemOutcome::Deleted,
            _ => ItemOutcome::DeleteFailed {
                detail: detail.clone(),
                source_present: true,
            },
        };
    }
}

/// Why an item is left as it is: skip it and go on, or end the run.
enum Halt {
    Skip(SkipReason),
    Stop(StopReason),
}

/// Every check right before `planned` changes. Returns a pruned cache's
/// freed-bytes size before its tool runs.
fn prepare(planned: &PlannedItem, ctx: &ExecContext<'_>) -> Result<Option<(u64, bool)>, Halt> {
    if ctx.is_cancelled() {
        return Err(Halt::Stop(StopReason::Cancelled));
    }
    // Before the slow checks, so a changed policy never spends a probe.
    policy_unchanged(ctx.policy).map_err(Halt::Stop)?;
    validate(planned, ctx).map_err(Halt::Skip)?;
    // A pruned cache is measured among the slow checks, so the checks below
    // also cover the time its walk takes.
    let before_prune =
        (planned.removal == Removal::RunTool(DesktopTool::Prune)).then(|| measure(planned, false));
    // After them, which can take long enough for the rules to change.
    policy_unchanged(ctx.policy).map_err(Halt::Stop)?;
    if names_a_file(planned) {
        protection_skip(ctx.policy, &planned.item.path).map_err(Halt::Skip)?;
    }
    // Last check before the mutation.
    if ctx.is_cancelled() {
        return Err(Halt::Stop(StopReason::Cancelled));
    }
    Ok(before_prune)
}

fn act_on(
    planned: &PlannedItem,
    ctx: &ExecContext<'_>,
    stopped: &mut Option<StopReason>,
) -> ItemOutcome {
    let before_prune = match prepare(planned, ctx) {
        Ok(before) => before,
        Err(Halt::Skip(reason)) => return ItemOutcome::Skipped(reason),
        Err(Halt::Stop(reason)) => {
            *stopped = Some(reason);
            return ItemOutcome::NotAttempted;
        }
    };
    let path = &planned.item.path;
    // One route per item: a failure is reported, never retried another way.
    let finder = match planned.removal {
        Removal::MoveToTrash => ctx
            .trash
            .move_to_trash(path)
            .map(|()| ItemOutcome::MovedToTrash),
        Removal::EmptyTrash => ctx.trash.empty_trash().map(|()| ItemOutcome::TrashEmptied),
        Removal::Delete => {
            return ctx.delete.delete(path).map_or_else(
                |error| ItemOutcome::DeleteFailed {
                    detail: error.to_string(),
                    source_present: fs::symlink_metadata(path).is_ok(),
                },
                |()| ItemOutcome::Deleted,
            )
        }
        Removal::RunTool(_) => return run_tool(planned, before_prune, ctx),
        // Only `delete_as_administrator` acts on these, as one batch.
        Removal::DeleteAsAdministrator => return ItemOutcome::NotAttempted,
    };
    finder.unwrap_or_else(|error| {
        if matches!(error, TrashError::AutomationDenied(_)) {
            *stopped = Some(StopReason::AutomationDenied);
        }
        ItemOutcome::Failed {
            error,
            source_present: fs::symlink_metadata(path).is_ok(),
        }
    })
}

/// The freed bytes of `planned`'s folder by the freed-bytes rule, and
/// whether that is only an upper bound. `unreadable_is_upper_bound` counts a
/// walk that could not read everything as one too: after a prune such a size
/// is too low, so the bytes freed (before minus after) would read too high.
fn measure(planned: &PlannedItem, unreadable_is_upper_bound: bool) -> (u64, bool) {
    let scan = ScanContext::unchecked();
    let bytes = dir_size_checked(&planned.item.path, &scan);
    let findings = scan.take_findings();
    let upper_bound = !findings.size_upper_bound.is_empty()
        || (unreadable_is_upper_bound && !findings.unreadable.is_empty());
    (bytes, upper_bound)
}

/// Runs the category's own tool for `planned`, once: a prune when its cache
/// was measured `before`, a requested removal otherwise. A prune is measured
/// again afterwards; a failure is the item's outcome and nothing else is
/// tried.
fn run_tool(
    planned: &PlannedItem,
    before: Option<(u64, bool)>,
    ctx: &ExecContext<'_>,
) -> ItemOutcome {
    let item = &planned.item;
    let Some(provider) = ctx.providers.iter().find(|p| p.id() == item.category_id) else {
        return ItemOutcome::Skipped(SkipReason::UnknownCategory);
    };
    if let Err(detail) = provider.run_desktop_tool(item) {
        return ItemOutcome::ToolFailed { detail };
    }
    match before {
        Some((before, upper_before)) => {
            let (after, upper_after) = measure(planned, true);
            ItemOutcome::Pruned {
                bytes_freed: before.saturating_sub(after),
                upper_bound: upper_before || upper_after,
            }
        }
        None => ItemOutcome::RemovalRequested,
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
        return Err(SkipReason::ProtectedPath("inside /System".into()));
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
