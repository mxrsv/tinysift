//! The desktop's cleanup: each planned path is revalidated right before it
//! is moved to the Trash, deleted for good or, for the Trash row, emptied,
//! as its plan says (`Removal`), and every path gets its own outcome. No
//! failure is retried another way. It never calls `CleanProvider::execute`,
//! so no provider command runs from here.
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

use super::finder_trash::{Trash, TrashError};
use super::fs_safe::{Delete, PathFingerprint};
use super::process::AppProbe;
use super::protection::{Evaluation, Matcher, Relation, INTERNAL_ID_PREFIX};
use super::protection_store::{StoreError, StoreLease};
use super::providers::{desktop_report_only, CleanProvider};
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
    /// Deleted for good (`Removal::Delete`).
    Deleted,
    /// Finder emptied the Trash (`Removal::EmptyTrash`).
    TrashEmptied,
    /// Finder refused to move the path or to empty the Trash.
    /// `source_present` is re-checked afterwards.
    Failed {
        error: TrashError,
        source_present: bool,
    },
    /// Permanent deletion failed; part of the path may be gone. Nothing else
    /// is tried: no Trash, no other route.
    DeleteFailed {
        detail: String,
        source_present: bool,
    },
    Skipped(SkipReason),
    /// Execution stopped before this path.
    NotAttempted,
}

impl ItemOutcome {
    /// Deleted, or the Trash emptied: the item's bytes are free now.
    pub fn frees_space(&self) -> bool {
        matches!(self, Self::Deleted | Self::TrashEmptied)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    Cancelled,
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
    /// between them counted once (`freed_together`).
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
}

/// Removes each planned path as planned after `validate` passes for it.
/// Permanent removals run first, so emptying the Trash never takes what
/// this run moves there; results stay in plan order. Stops on cancellation,
/// an Automation denial or a change in the saved rules; the remaining paths
/// are reported as not attempted.
pub fn execute_checked(plan: &[PlannedItem], ctx: &ExecContext<'_>) -> CheckedExecReport {
    let total = plan.len() as u64;
    let mut stopped = None;
    let mut outcomes = vec![ItemOutcome::NotAttempted; plan.len()];
    for (step, index) in execution_order(plan).into_iter().enumerate() {
        if stopped.is_some() {
            break;
        }
        let planned = &plan[index];
        crate::progress::report(
            ctx.progress,
            "trash",
            step as u64,
            Some(total),
            &planned.item.path.to_string_lossy(),
        );
        outcomes[index] = act_on(planned, ctx, &mut stopped);
    }
    if stopped.is_none() {
        crate::progress::report(ctx.progress, "trash", total, Some(total), "Done");
    }
    let (bytes_freed, freed_is_upper_bound) = freed_together(
        plan.iter()
            .zip(&outcomes)
            .filter(|(_, outcome)| outcome.frees_space())
            .map(|(planned, _)| planned),
    );
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

/// Plan indexes in run order: deletions, then emptying the Trash, then
/// moves to the Trash, each in plan order.
fn execution_order(plan: &[PlannedItem]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..plan.len()).collect();
    order.sort_by_key(|&index| match plan[index].removal {
        Removal::Delete => 0,
        Removal::EmptyTrash => 1,
        Removal::MoveToTrash => 2,
    });
    order
}

fn act_on(
    planned: &PlannedItem,
    ctx: &ExecContext<'_>,
    stopped: &mut Option<StopReason>,
) -> ItemOutcome {
    let mut stop = |reason: StopReason| {
        *stopped = Some(reason);
        ItemOutcome::NotAttempted
    };
    if ctx.is_cancelled() {
        return stop(StopReason::Cancelled);
    }
    // Before the slow checks, so a changed policy never spends a probe.
    if let Err(reason) = policy_unchanged(ctx.policy) {
        return stop(reason);
    }
    if let Err(reason) = validate(planned, ctx) {
        return ItemOutcome::Skipped(reason);
    }
    // After them, which can take long enough for the rules to change.
    if let Err(reason) = policy_unchanged(ctx.policy) {
        return stop(reason);
    }
    if let Err(reason) = protection_skip(ctx.policy, &planned.item.path) {
        return ItemOutcome::Skipped(reason);
    }
    // Last check before the mutation.
    if ctx.is_cancelled() {
        return stop(StopReason::Cancelled);
    }
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
    protection_skip(ctx.policy, &item.path)?;
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
    let meta = fs::symlink_metadata(&item.path).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => SkipReason::Missing,
        _ => SkipReason::SafetyCheckFailed(e.to_string()),
    })?;
    if meta.file_type().is_symlink() {
        return Err(SkipReason::Symlink);
    }
    let fingerprint = match planned.removal {
        Removal::EmptyTrash => PathFingerprint::of_listing(&meta),
        Removal::MoveToTrash | Removal::Delete => PathFingerprint::of(&meta),
    };
    if fingerprint != planned.fingerprint {
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

/// Device of the home folder, whose volume is known to have a Trash.
pub(crate) fn home_device(home: Option<&Path>) -> Result<u64, SkipReason> {
    let home = home.ok_or_else(|| SkipReason::SafetyCheckFailed("home folder unknown".into()))?;
    fs::metadata(home)
        .map(|m| m.dev())
        .map_err(|e| SkipReason::SafetyCheckFailed(format!("cannot stat home folder: {e}")))
}

/// The item and every item merged into it, each with its provider. Any
/// unknown or report-only provider refuses the whole removal; only the Trash
/// row's own provider may empty the Trash, and nothing else. A deletion
/// needs every one of them in Rebuilt automatically or Project build output
/// (`removal_for`), so a plan never deletes what only goes to the Trash.
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
            let allowed = match planned.removal {
                Removal::EmptyTrash if index == 0 => provider.desktop_empties_trash(),
                _ => desktop_report_only(provider.as_ref()).is_none(),
            };
            if !allowed {
                return Err(SkipReason::ReportOnly);
            }
            Ok((provider.as_ref(), item.path.as_path()))
        })
        .collect::<Result<Vec<_>, _>>()?;
    if planned.removal == Removal::Delete
        && removal_for(&planned.item, &planned.covers, DeleteMode::Permanent) != Removal::Delete
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
