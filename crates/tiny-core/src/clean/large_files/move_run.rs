//! Moving picked Large Files to the Trash: each path is checked again right
//! before it moves, best effort per file (RM1), every outcome carries its
//! cause (RM2). It never deletes: the only route is [`Trash`], and the
//! caller builds the plan from its own scan, never from a client path.
//!
//! The checks are Clean's own pieces, in Clean's order: the saved "always
//! keep" rules (held for the whole run through the lease, a change stops the
//! run), `/System`, the path still the file the scan saw (size and mtime for
//! a file), not a symlink, on the home volume, inside a folder the scan
//! looked at through real folders, and not a protected path. A file's
//! open-file check is the plan's backlog item, not done here.

use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::clean::checked_execute::{
    ancestors_are_real_dirs, home_device, policy_unchanged, protection_skip, SkipReason, StopReason,
};
use crate::clean::failure_cause::{skip_failure, trash_failure, Failure};
use crate::clean::finder_trash::{Trash, TrashError};
use crate::clean::fs_safe::PathFingerprint;
use crate::clean::private_size::private_allocated_bytes;
use crate::clean::protection_store::StoreLease;
use crate::clean::trash_plan::{inside_system, protected_reason};
use crate::progress::ProgressCallback;

/// One path to move, as the scan recorded it.
#[derive(Debug, Clone)]
pub struct PlannedMove {
    pub path: PathBuf,
    pub fingerprint: PathFingerprint,
    /// A file that must still be there, unchanged, when this one moves: the
    /// original of a Copies row (CP1). Without it the move is skipped, so a
    /// copy never goes while its original is gone.
    pub must_remain: Option<(PathBuf, PathFingerprint)>,
}

/// Everything a run needs besides the plan.
pub struct MoveContext<'a> {
    /// The trusted home folder; its volume is known to have a Trash.
    pub home: &'a Path,
    /// The folders the scan listed; a path must still lie inside one.
    pub roots: &'a [PathBuf],
    pub trash: &'a dyn Trash,
    pub cancel: Option<&'a AtomicBool>,
    pub progress: ProgressCallback<'a>,
    /// The saved rules, locked for the whole run.
    pub policy: &'a StoreLease<'a>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveOutcome {
    /// In the Trash, at `trashed_to` when the route said where.
    Moved {
        trashed_to: Option<PathBuf>,
    },
    /// The route refused; `source_present` is looked at again afterwards.
    Failed {
        error: TrashError,
        source_present: bool,
    },
    Skipped(SkipReason),
    /// The run stopped before this path.
    NotAttempted,
}

#[derive(Debug, Clone)]
pub struct MoveResult {
    pub path: PathBuf,
    pub outcome: MoveOutcome,
    /// Why it stayed; `None` when it moved, was not attempted, or a saved
    /// rule of the user kept it.
    pub failure: Option<Failure>,
    /// Measured right before a move that went through: private bytes, what
    /// emptying the Trash frees. A move frees nothing by itself (FG3), so
    /// this is not "freed". 0 for anything that did not move.
    pub bytes_moved: u64,
    /// `bytes_moved` may be high: the volume could not vouch for every block.
    pub bytes_moved_is_upper_bound: bool,
    /// The file as it was when it moved, so Undo can tell it from a
    /// replacement. Set only for a move.
    pub fingerprint: Option<PathFingerprint>,
}

#[derive(Debug, Clone, Default)]
pub struct MoveReport {
    /// One entry per planned item, in plan order.
    pub results: Vec<MoveResult>,
    pub stopped: Option<StopReason>,
}

impl MoveReport {
    pub fn moved_count(&self) -> usize {
        self.results
            .iter()
            .filter(|r| matches!(r.outcome, MoveOutcome::Moved { .. }))
            .count()
    }

    pub fn bytes_moved(&self) -> u64 {
        self.results
            .iter()
            .fold(0, |sum, r| sum.saturating_add(r.bytes_moved))
    }
}

/// Moves each planned path after `check` passes for it, in plan order. Stops
/// on cancellation, a denied Automation or a change in the saved rules; the
/// remaining paths are reported as not attempted. Every other refusal stays
/// with its own item.
pub fn move_to_trash(plan: &[PlannedMove], ctx: &MoveContext<'_>) -> MoveReport {
    let total = plan.len() as u64;
    let mut stopped = None;
    let mut results = Vec::with_capacity(plan.len());
    for (step, planned) in plan.iter().enumerate() {
        if stopped.is_some() {
            results.push(untouched(planned, MoveOutcome::NotAttempted));
            continue;
        }
        crate::progress::report(
            ctx.progress,
            "trash",
            step as u64,
            Some(total),
            &planned.path.to_string_lossy(),
        );
        results.push(move_one(planned, ctx, &mut stopped));
    }
    if stopped.is_none() {
        crate::progress::report(ctx.progress, "trash", total, Some(total), "Done");
    }
    MoveReport { results, stopped }
}

fn untouched(planned: &PlannedMove, outcome: MoveOutcome) -> MoveResult {
    MoveResult {
        path: planned.path.clone(),
        outcome,
        failure: None,
        bytes_moved: 0,
        bytes_moved_is_upper_bound: false,
        fingerprint: None,
    }
}

fn move_one(
    planned: &PlannedMove,
    ctx: &MoveContext<'_>,
    stopped: &mut Option<StopReason>,
) -> MoveResult {
    let path = planned.path.as_path();
    let stop = |reason, stopped: &mut Option<StopReason>| {
        *stopped = Some(reason);
        untouched(planned, MoveOutcome::NotAttempted)
    };
    if is_cancelled(ctx) {
        return stop(StopReason::Cancelled, stopped);
    }
    // Before the slow checks, so a changed policy never spends them.
    if let Err(reason) = policy_unchanged(ctx.policy) {
        return stop(reason, stopped);
    }
    if let Err(reason) = check(planned, ctx) {
        let failure = skip_failure(&reason);
        return MoveResult {
            failure,
            ..untouched(planned, MoveOutcome::Skipped(reason))
        };
    }
    let measured = private_allocated_bytes(path);
    // After the slow checks, which can take long enough for the rules to change.
    if let Err(reason) = policy_unchanged(ctx.policy) {
        return stop(reason, stopped);
    }
    if is_cancelled(ctx) {
        return stop(StopReason::Cancelled, stopped);
    }
    // One route: a failure is reported, never retried another way.
    match ctx.trash.move_to_trash_reporting(path) {
        Ok(trashed_to) => {
            let (bytes, exact) = measured.map_or((0, false), |m| (m.bytes, m.exact));
            MoveResult {
                path: planned.path.clone(),
                outcome: MoveOutcome::Moved { trashed_to },
                failure: None,
                bytes_moved: bytes,
                bytes_moved_is_upper_bound: !exact,
                fingerprint: Some(planned.fingerprint.clone()),
            }
        }
        Err(error) => {
            if matches!(error, TrashError::AutomationDenied(_)) {
                *stopped = Some(StopReason::AutomationDenied);
            }
            MoveResult {
                path: planned.path.clone(),
                failure: Some(trash_failure(path, &error)),
                outcome: MoveOutcome::Failed {
                    error,
                    source_present: fs::symlink_metadata(path).is_ok(),
                },
                bytes_moved: 0,
                bytes_moved_is_upper_bound: false,
                fingerprint: None,
            }
        }
    }
}

fn is_cancelled(ctx: &MoveContext<'_>) -> bool {
    ctx.cancel.is_some_and(|flag| flag.load(Ordering::Acquire))
}

/// The per-item validator, run immediately before each move.
fn check(planned: &PlannedMove, ctx: &MoveContext<'_>) -> Result<(), SkipReason> {
    let path = planned.path.as_path();
    if inside_system(path) {
        return Err(SkipReason::ProtectedPath(
            crate::clean::failure_cause::INSIDE_SYSTEM.into(),
        ));
    }
    if !path.is_absolute() {
        return Err(SkipReason::Missing);
    }
    protection_skip(ctx.policy, path)?;
    let meta = fs::symlink_metadata(path).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => SkipReason::Missing,
        _ => SkipReason::SafetyCheckFailed(e.to_string()),
    })?;
    if meta.file_type().is_symlink() {
        return Err(SkipReason::Symlink);
    }
    if PathFingerprint::of(&meta) != planned.fingerprint {
        return Err(SkipReason::Changed);
    }
    if let Some((keeper, fingerprint)) = &planned.must_remain {
        match fs::symlink_metadata(keeper) {
            Ok(m) if m.is_file() && PathFingerprint::of(&m) == *fingerprint => {}
            _ => return Err(SkipReason::Changed),
        }
    }
    if meta.dev() != home_device(Some(ctx.home))? {
        return Err(SkipReason::NotOnHomeVolume);
    }
    let inside = ctx
        .roots
        .iter()
        .any(|root| path.starts_with(root) && ancestors_are_real_dirs(path));
    if !inside {
        return Err(SkipReason::OutsideRoots);
    }
    match protected_reason(path, Some(ctx.home), false) {
        Some(reason) => Err(SkipReason::ProtectedPath(reason)),
        None => Ok(()),
    }
}
