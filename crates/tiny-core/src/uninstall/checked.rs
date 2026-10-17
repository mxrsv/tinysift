//! The desktop's checked uninstall: the app bundle and its reviewed leftovers
//! move to Trash only after each one is revalidated right before its move.
//!
//! Counterpart of [`execute_checked`](crate::clean::checked_execute::execute_checked)
//! for uninstall. Execution never takes a path from the caller: it takes the
//! [`CheckedPlan`] built at discovery plus a set of item indexes. There is no
//! hard-delete path, no retry and no replay.

use std::collections::BTreeSet;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use super::{ItemKind, Ownership, Plan};
use crate::clean::checked_execute::{
    ancestors_are_real_dirs, home_device, policy_unchanged, protection_skip, ItemOutcome,
    SkipReason, StopReason as CleanStop,
};
use crate::clean::finder_trash::{Trash, TrashError};
use crate::clean::fs_safe::PathFingerprint;
use crate::clean::process::AppProbe;
use crate::clean::protection_store::StoreLease;
use crate::clean::trash_plan::protected_reason;
use crate::progress::ProgressCallback;
use crate::uninstall::Consent;

/// Progress operation name.
const OPERATION: &str = "uninstall";

/// One item as discovery saw it, with the container it must stay inside.
#[derive(Debug, Clone)]
pub struct CheckedItem {
    path: PathBuf,
    size: u64,
    kind: ItemKind,
    ownership: Ownership,
    /// Taken at discovery with `symlink_metadata`. An item that could not
    /// be read then keeps the reason and is skipped at execution.
    fingerprint: Result<PathFingerprint, SkipReason>,
    /// The path must lie strictly inside this folder: the installed-app root
    /// for the bundle, `<home>/Library` for a leftover.
    container: PathBuf,
}

impl CheckedItem {
    pub fn path(&self) -> &Path {
        &self.path
    }
    pub fn size(&self) -> u64 {
        self.size
    }
    pub fn kind(&self) -> ItemKind {
        self.kind
    }
    pub fn ownership(&self) -> &Ownership {
        &self.ownership
    }
}

/// What execution is allowed to act on. Items are addressed by their index.
#[derive(Debug, Clone)]
pub struct CheckedPlan {
    /// The app's process name (`CFBundleExecutable`), read at discovery.
    /// `None` makes every run refuse: the running-app check cannot be named.
    executable: Option<String>,
    items: Vec<CheckedItem>,
}

impl CheckedPlan {
    pub fn items(&self) -> &[CheckedItem] {
        &self.items
    }
}

/// A run that was refused before anything moved.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UninstallRefusal {
    #[error("{0}")]
    Blocked(String),
    #[error("cannot read {path}: {error}")]
    Unreadable { path: PathBuf, error: String },
    #[error("the plan is inconsistent: {0}")]
    InconsistentPlan(String),
    #[error("nothing is selected")]
    NothingSelected,
    #[error("item {0} is not in the plan")]
    UnknownItem(usize),
    #[error("item {index} cannot be removed: {reason}")]
    NotSelectable { index: usize, reason: String },
    #[error("the app's process name is unknown, so a running copy cannot be ruled out")]
    ExecutableUnknown,
    #[error("{0} is running")]
    AppRunning(String),
    #[error("running-app check for {name} failed: {error}")]
    AppCheckFailed { name: String, error: String },
}

/// Freezes `plan` for execution. `executable` is the app's process name, read
/// by the caller with `read_bundle_executable` (an unreadable one is `None`).
/// `applications_root` is the installed-app folder the bundle must sit in.
/// Every plan item keeps its index; a leftover that cannot be read now is
/// skipped at execution, never guessed at. Build only from a `Plan` core
/// built at discovery, never from client input.
pub fn build_checked_plan(
    plan: &Plan,
    home: &Path,
    applications_root: &Path,
    executable: Option<String>,
) -> Result<CheckedPlan, UninstallRefusal> {
    if let Some(reason) = &plan.blocked {
        return Err(UninstallRefusal::Blocked(reason.clone()));
    }
    ensure_consistent(plan)?;
    let library = home.join("Library");
    let mut items = Vec::new();
    for item in &plan.items {
        let container = match item.kind {
            ItemKind::AppBundle => applications_root,
            ItemKind::Leftover => library.as_path(),
        };
        let fingerprint = match fs::symlink_metadata(&item.path) {
            Ok(meta) => Ok(PathFingerprint::of(&meta)),
            Err(error) if item.kind == ItemKind::AppBundle => {
                return Err(UninstallRefusal::Unreadable {
                    path: item.path.clone(),
                    error: error.to_string(),
                })
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => Err(SkipReason::Missing),
            Err(error) => Err(SkipReason::SafetyCheckFailed(error.to_string())),
        };
        items.push(CheckedItem {
            path: item.path.clone(),
            size: item.size,
            kind: item.kind,
            ownership: item.ownership.clone(),
            fingerprint,
            container: container.to_path_buf(),
        });
    }
    let executable = executable
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty() && !name.contains(['/', '\0']));
    Ok(CheckedPlan { executable, items })
}

/// One bundle at most, and `AppBundle` ownership exactly on bundle items.
fn ensure_consistent(plan: &Plan) -> Result<(), UninstallRefusal> {
    let bundles = plan
        .items
        .iter()
        .filter(|i| i.kind == ItemKind::AppBundle)
        .count();
    let mismatch = plan
        .items
        .iter()
        .any(|i| (i.kind == ItemKind::AppBundle) != (i.ownership == Ownership::AppBundle));
    match (bundles > 1, mismatch) {
        (true, _) => Err(UninstallRefusal::InconsistentPlan(
            "more than one app bundle".into(),
        )),
        (_, true) => Err(UninstallRefusal::InconsistentPlan(
            "an item's kind and ownership disagree".into(),
        )),
        _ => Ok(()),
    }
}

/// Everything execution needs besides the plan and the selection.
pub struct UninstallContext<'a> {
    /// The home folder (`trash_plan::trusted_home` in the app).
    pub home: &'a Path,
    pub probe: &'a dyn AppProbe,
    pub trash: &'a dyn Trash,
    pub cancel: Option<&'a AtomicBool>,
    pub progress: ProgressCallback<'a>,
    /// The saved "always keep" rules, locked for the whole run.
    pub policy: &'a StoreLease<'a>,
}

impl UninstallContext<'_> {
    fn is_cancelled(&self) -> bool {
        self.cancel.is_some_and(|flag| flag.load(Ordering::Acquire))
    }
}

/// Why the run stopped before its last item.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopReason {
    Cancelled,
    AutomationDenied,
    /// The saved rules differ from what this run started with.
    ProtectionChanged,
    /// The saved rules could not be read again.
    ProtectionUnavailable(String),
    /// The app started after the run began.
    AppRunning(String),
    /// The running-app check failed mid-run, so acting would be a guess.
    AppCheckFailed(String),
}

impl From<CleanStop> for StopReason {
    fn from(stop: CleanStop) -> Self {
        match stop {
            CleanStop::Cancelled => Self::Cancelled,
            CleanStop::AutomationDenied => Self::AutomationDenied,
            CleanStop::ProtectionChanged => Self::ProtectionChanged,
            CleanStop::ProtectionUnavailable(why) => Self::ProtectionUnavailable(why),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ItemResult {
    /// Index into [`CheckedPlan::items`].
    pub index: usize,
    pub path: PathBuf,
    pub kind: ItemKind,
    pub size: u64,
    pub outcome: ItemOutcome,
}

#[derive(Debug, Clone, Default)]
pub struct UninstallReport {
    /// One entry per selected item, in execution order (bundle first).
    pub results: Vec<ItemResult>,
    pub stopped: Option<StopReason>,
}

impl UninstallReport {
    pub fn moved_count(&self) -> usize {
        self.results
            .iter()
            .filter(|r| r.outcome == ItemOutcome::MovedToTrash)
            .count()
    }

    /// Sizes measured at discovery of the items moved to Trash. Moving to
    /// Trash frees no space until Trash is emptied.
    pub fn bytes_moved_to_trash(&self) -> u64 {
        self.results
            .iter()
            .filter(|r| r.outcome == ItemOutcome::MovedToTrash)
            .fold(0, |sum, r| sum.saturating_add(r.size))
    }
}

/// Moves the selected items to Trash, the bundle first so that a failed bundle
/// move stops the run before any leftover is lost. Refuses (and moves nothing)
/// when the selection names a report-only or uncertain item, the app's process
/// name is unknown, or the app is running. Stops on cancellation, an Automation
/// denial, a change in the saved rules or the app starting.
pub fn execute_checked_uninstall(
    plan: &CheckedPlan,
    selection: &BTreeSet<usize>,
    ctx: &UninstallContext<'_>,
) -> Result<UninstallReport, UninstallRefusal> {
    let order = execution_order(plan, selection)?;
    let app = plan
        .executable
        .as_deref()
        .ok_or(UninstallRefusal::ExecutableUnknown)?;
    match ctx.probe.probe(app) {
        Ok(false) => {}
        Ok(true) => return Err(UninstallRefusal::AppRunning(app.into())),
        Err(error) => {
            return Err(UninstallRefusal::AppCheckFailed {
                name: app.into(),
                error,
            })
        }
    }
    let total = order.len() as u64;
    let mut report = UninstallReport::default();
    let mut bundle_blocked = false;
    for (position, &index) in order.iter().enumerate() {
        let item = &plan.items[index];
        let outcome = if report.stopped.is_some() || bundle_blocked {
            ItemOutcome::NotAttempted
        } else {
            let shown = item.path.to_string_lossy();
            crate::progress::report(
                ctx.progress,
                OPERATION,
                position as u64,
                Some(total),
                &shown,
            );
            act_on(item, app, ctx, &mut report.stopped)
        };
        // A bundle that did not move must not let its data go first.
        bundle_blocked |= item.kind == ItemKind::AppBundle && outcome != ItemOutcome::MovedToTrash;
        report.results.push(ItemResult {
            index,
            path: item.path.clone(),
            kind: item.kind,
            size: item.size,
            outcome,
        });
    }
    if report.stopped.is_none() {
        crate::progress::report(ctx.progress, OPERATION, total, Some(total), "Done");
    }
    Ok(report)
}

/// The selected indexes, the bundle first and then plan order. Selecting
/// anything an explicit choice may not remove refuses the whole run.
fn execution_order(
    plan: &CheckedPlan,
    selection: &BTreeSet<usize>,
) -> Result<Vec<usize>, UninstallRefusal> {
    if selection.is_empty() {
        return Err(UninstallRefusal::NothingSelected);
    }
    for &index in selection {
        let item = plan
            .items
            .get(index)
            .ok_or(UninstallRefusal::UnknownItem(index))?;
        if !item.ownership.allows(Consent::Explicit) {
            let reason = match &item.ownership {
                Ownership::ReportOnly { reason } | Ownership::Uncertain { reason } => {
                    reason.clone()
                }
                _ => "not removable".into(),
            };
            return Err(UninstallRefusal::NotSelectable { index, reason });
        }
    }
    let mut order: Vec<usize> = selection.iter().copied().collect();
    order.sort_by_key(|&i| (plan.items[i].kind != ItemKind::AppBundle, i));
    Ok(order)
}

/// Why one item is not moved: skip it and go on, or end the run.
enum Halt {
    Skip(SkipReason),
    Stop(StopReason),
}

impl From<SkipReason> for Halt {
    fn from(reason: SkipReason) -> Self {
        Self::Skip(reason)
    }
}

fn act_on(
    item: &CheckedItem,
    app: &str,
    ctx: &UninstallContext<'_>,
    stopped: &mut Option<StopReason>,
) -> ItemOutcome {
    match check_item(item, app, ctx) {
        Ok(()) => {}
        Err(Halt::Skip(reason)) => return ItemOutcome::Skipped(reason),
        Err(Halt::Stop(reason)) => {
            *stopped = Some(reason);
            return ItemOutcome::NotAttempted;
        }
    }
    match ctx.trash.move_to_trash(&item.path) {
        Ok(()) => ItemOutcome::MovedToTrash,
        Err(error) => {
            if matches!(error, TrashError::AutomationDenied(_)) {
                *stopped = Some(StopReason::AutomationDenied);
            }
            ItemOutcome::Failed {
                error,
                source_present: fs::symlink_metadata(&item.path).is_ok(),
            }
        }
    }
}

/// Everything that must hold immediately before `item` moves.
fn check_item(item: &CheckedItem, app: &str, ctx: &UninstallContext<'_>) -> Result<(), Halt> {
    if ctx.is_cancelled() {
        return Err(Halt::Stop(StopReason::Cancelled));
    }
    policy_unchanged(ctx.policy).map_err(|stop| Halt::Stop(stop.into()))?;
    protection_skip(ctx.policy, &item.path)?;
    match ctx.probe.probe(app) {
        Ok(false) => {}
        Ok(true) => return Err(Halt::Stop(StopReason::AppRunning(app.into()))),
        Err(error) => return Err(Halt::Stop(StopReason::AppCheckFailed(error))),
    }
    check_path(item, ctx)?;
    policy_unchanged(ctx.policy).map_err(|stop| Halt::Stop(stop.into()))?;
    // The rules can change while the slow checks run.
    protection_skip(ctx.policy, &item.path)?;
    if ctx.is_cancelled() {
        return Err(Halt::Stop(StopReason::Cancelled));
    }
    Ok(())
}

/// The path checks, kept together so they sit right before the move.
fn check_path(item: &CheckedItem, ctx: &UninstallContext<'_>) -> Result<(), SkipReason> {
    let meta = fs::symlink_metadata(&item.path).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => SkipReason::Missing,
        _ => SkipReason::SafetyCheckFailed(e.to_string()),
    })?;
    if meta.file_type().is_symlink() {
        return Err(SkipReason::Symlink);
    }
    if item.fingerprint.as_ref().map_err(Clone::clone)? != &PathFingerprint::of(&meta) {
        return Err(SkipReason::Changed);
    }
    if !ancestors_are_real_dirs(&item.path) || !strictly_inside(&item.path, &item.container) {
        return Err(SkipReason::OutsideRoots);
    }
    if let Some(reason) = protected_reason(&item.path, Some(ctx.home), false) {
        return Err(SkipReason::ProtectedPath(reason));
    }
    // Finder may delete permanently on a volume without a Trash, so nothing
    // moves from another volume than the home folder, the bundle included.
    if meta.dev() != home_device(Some(ctx.home))? {
        return Err(SkipReason::NotOnHomeVolume);
    }
    Ok(())
}

/// `path` is below `container`, compared one component at a time, and every
/// component after it is a plain name (no `..` or `.`).
fn strictly_inside(path: &Path, container: &Path) -> bool {
    path.strip_prefix(container).is_ok_and(|rest| {
        rest.components().next().is_some()
            && rest.components().all(|c| matches!(c, Component::Normal(_)))
    })
}

#[cfg(test)]
mod tests;
