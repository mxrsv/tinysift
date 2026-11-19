//! Moving agent worktrees to the Trash (the app's feature-upgrades spec,
//! UG-W8 and UG-W9). Each planned worktree is its own root: right before it
//! moves it must still be the folder that was listed (same device and inode,
//! still this repository's, same branch and commit), pass Clean's path
//! checks (not in `/System`, no symlink on the way, on the home volume, not
//! the home folder or above it, not held by a saved "always keep" rule) and
//! still have a status that can be ticked. A row that became locked is
//! skipped with the status it has now. After a repository's moves, git is
//! asked once to `worktree prune`, so the branches are free at once, but only
//! when its dry run would forget exactly the worktrees this run moved:
//! another worktree whose folder is missing (on a disk that is not plugged
//! in, or moved by hand) keeps its `HEAD`, index and reflog.
//!
//! Nothing here deletes: the only route is [`Trash`]. A failure stays with
//! its own worktree and is never retried another way.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use super::repo::{self, Entry};
use super::status::{self, BusyFolders, Context, WorktreeStatus};
use crate::clean::checked_execute::{
    ancestors_are_real_dirs, home_device, policy_unchanged, protection_skip, SkipReason, StopReason,
};
use crate::clean::failure_cause::INSIDE_SYSTEM;
use crate::clean::finder_trash::{Trash, TrashError};
use crate::clean::protection_store::StoreLease;
use crate::clean::providers::project_activity::{exit_detail, run_git};
use crate::clean::trash_plan::{inside_system, protected_reason};
use crate::progress::ProgressCallback;
use crate::runner::CommandRunner;

/// The progress step reported before each worktree is checked and moved.
pub const PROGRESS_MOVING: &str = "worktree-trash";
/// The progress step reported right after a worktree went to the Trash; its
/// message is the worktree's path, so a list can drop that row at once.
pub const PROGRESS_MOVED: &str = "worktree-moved";

/// A worktree as the listing saw it: what the move must find again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannedWorktree {
    /// Its repository's git directory.
    pub git_dir: PathBuf,
    /// As git listed it.
    pub path: PathBuf,
    pub branch: Option<String>,
    pub head: String,
    /// The folder's device and inode when it was listed.
    pub folder: (u64, u64),
}

impl PlannedWorktree {
    /// The worktree `entry` of `git_dir` as it stands now; `None` when its
    /// folder cannot be read.
    pub fn of(git_dir: &Path, path: &Path, branch: Option<String>, head: String) -> Option<Self> {
        let meta = fs::symlink_metadata(path).ok()?;
        Some(Self {
            git_dir: git_dir.to_path_buf(),
            path: path.to_path_buf(),
            branch,
            head,
            folder: (meta.dev(), meta.ino()),
        })
    }
}

/// What a move reads again right before each worktree, so a process that
/// started inside it since the listing locks it.
pub trait Machine {
    /// The working directory of every process that can be read.
    fn working_directories(&self) -> Vec<PathBuf>;
    /// Whether an agent session runs whose folder cannot be read.
    fn unreadable_session(&self) -> bool;
}

/// Everything a run needs besides the plan.
pub struct MoveInputs<'a> {
    pub runner: &'a dyn CommandRunner,
    /// The trusted home folder; its volume is the one with a Trash.
    pub home: &'a Path,
    pub trash: &'a dyn Trash,
    pub machine: &'a dyn Machine,
    /// The saved rules, locked for the whole run.
    pub policy: &'a StoreLease<'a>,
    pub cancel: Option<&'a AtomicBool>,
    pub progress: ProgressCallback<'a>,
    /// Unix seconds, for a Last change row's age.
    pub now: i64,
}

/// Why a worktree stayed where it was without the Trash being asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Refusal {
    /// It can no longer be ticked: the status it has now.
    Locked(WorktreeStatus),
    /// Its folder is gone.
    Gone,
    /// Not the worktree that was listed: another folder, branch or commit,
    /// or no longer this repository's.
    Changed,
    /// A saved "always keep" rule holds it, or something inside it.
    Kept,
    /// A path check refused it or could not run; English detail.
    Unsafe(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MoveOutcome {
    Moved,
    Refused(Refusal),
    /// The Trash refused; the folder is looked at again afterwards.
    Failed {
        error: TrashError,
        source_present: bool,
    },
    /// The run stopped before this worktree.
    NotAttempted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MoveResult {
    pub path: PathBuf,
    pub outcome: MoveOutcome,
}

/// `git worktree prune` that did not succeed after a repository's moves:
/// its folders are in the Trash, but git still lists them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PruneFailure {
    pub git_dir: PathBuf,
    pub detail: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MoveReport {
    /// One per planned worktree, in plan order.
    pub results: Vec<MoveResult>,
    pub prune_failures: Vec<PruneFailure>,
    pub stopped: Option<StopReason>,
}

/// Moves each planned worktree to the Trash after its checks pass, one
/// repository at a time in the order they first appear, and prunes each
/// repository that lost a worktree. Stops on cancellation, a denied
/// Automation or a change in the saved rules; the rest are not attempted,
/// and a repository whose moves started is still pruned.
pub fn move_to_trash(plan: &[PlannedWorktree], inputs: &MoveInputs<'_>) -> MoveReport {
    let total = plan.len() as u64;
    let mut outcomes = vec![MoveOutcome::NotAttempted; plan.len()];
    let mut prune_failures = Vec::new();
    let mut stopped = None;
    let mut step = 0u64;
    for git_dir in repositories(plan) {
        if stopped.is_some() {
            break;
        }
        let base = status::base_branch(inputs.runner, &git_dir);
        let mut moved_any = false;
        // The `worktrees/` entries of the worktrees that moved.
        let mut moved_entries = HashSet::new();
        for (index, planned) in plan.iter().enumerate() {
            if planned.git_dir != git_dir || stopped.is_some() {
                continue;
            }
            report(inputs, PROGRESS_MOVING, step, total, &planned.path);
            step += 1;
            outcomes[index] = move_one(
                planned,
                base.as_deref(),
                inputs,
                &mut stopped,
                &mut moved_entries,
            );
            if outcomes[index] == MoveOutcome::Moved {
                moved_any = true;
                report(inputs, PROGRESS_MOVED, step, total, &planned.path);
            }
        }
        if moved_any {
            if let Err(detail) = prune(inputs.runner, &git_dir, &moved_entries) {
                prune_failures.push(PruneFailure { git_dir, detail });
            }
        }
    }
    let results = plan
        .iter()
        .zip(outcomes)
        .map(|(planned, outcome)| MoveResult {
            path: planned.path.clone(),
            outcome,
        })
        .collect();
    MoveReport {
        results,
        prune_failures,
        stopped,
    }
}

fn report(inputs: &MoveInputs<'_>, operation: &str, step: u64, total: u64, path: &Path) {
    let path = path.to_string_lossy();
    crate::progress::report(inputs.progress, operation, step, Some(total), &path);
}

/// Each git directory of the plan once, in the order it first appears.
fn repositories(plan: &[PlannedWorktree]) -> Vec<PathBuf> {
    let mut order: Vec<PathBuf> = Vec::new();
    for planned in plan {
        if !order.contains(&planned.git_dir) {
            order.push(planned.git_dir.clone());
        }
    }
    order
}

fn move_one(
    planned: &PlannedWorktree,
    base: Option<&str>,
    inputs: &MoveInputs<'_>,
    stopped: &mut Option<StopReason>,
    moved_entries: &mut HashSet<String>,
) -> MoveOutcome {
    let mut stop = |reason| {
        *stopped = Some(reason);
        MoveOutcome::NotAttempted
    };
    if is_cancelled(inputs) {
        return stop(StopReason::Cancelled);
    }
    // Before the slow checks, so a changed policy never spends them.
    if let Err(reason) = policy_unchanged(inputs.policy) {
        return stop(reason);
    }
    if let Err(refusal) = check(planned, base, inputs) {
        return MoveOutcome::Refused(refusal);
    }
    // git took a while: the rules or the user may have changed their mind.
    if let Err(reason) = policy_unchanged(inputs.policy) {
        return stop(reason);
    }
    if is_cancelled(inputs) {
        return stop(StopReason::Cancelled);
    }
    // Read while the folder is here: the prune may forget only these.
    let entry = admin_entry(&planned.path);
    // One route: a failure is reported, never retried another way.
    match inputs.trash.move_to_trash(&planned.path) {
        Ok(()) => {
            moved_entries.extend(entry);
            MoveOutcome::Moved
        }
        Err(error) => {
            if matches!(error, TrashError::AutomationDenied(_)) {
                *stopped = Some(StopReason::AutomationDenied);
            }
            MoveOutcome::Failed {
                error,
                source_present: fs::symlink_metadata(&planned.path).is_ok(),
            }
        }
    }
}

fn is_cancelled(inputs: &MoveInputs<'_>) -> bool {
    inputs
        .cancel
        .is_some_and(|flag| flag.load(Ordering::Acquire))
}

/// The checks right before a move: the path's own first (cheap, and they
/// make sure git is asked about the folder that was listed), then git's
/// fresh listing of the repository, then the status table.
fn check(
    planned: &PlannedWorktree,
    base: Option<&str>,
    inputs: &MoveInputs<'_>,
) -> Result<(), Refusal> {
    let path = planned.path.as_path();
    check_path(planned, inputs)?;
    let entry = listed_again(planned, inputs.runner)?;
    let context = Context {
        runner: inputs.runner,
        home: inputs.home,
        busy: BusyFolders::new(&inputs.machine.working_directories()),
        unreadable_session: inputs.machine.unreadable_session(),
        now: inputs.now,
    };
    let status = status::judge(&context, &entry, base);
    if status.is_locked() {
        return Err(Refusal::Locked(status));
    }
    // git ran for a while: the folder must still be the one it judged.
    check_folder(planned, path)
}

fn check_path(planned: &PlannedWorktree, inputs: &MoveInputs<'_>) -> Result<(), Refusal> {
    let path = planned.path.as_path();
    if !path.is_absolute() || inside_system(path) {
        return Err(Refusal::Unsafe(INSIDE_SYSTEM.into()));
    }
    check_folder(planned, path)?;
    protection_skip(inputs.policy, path).map_err(refusal_of)?;
    if !ancestors_are_real_dirs(path) {
        return Err(Refusal::Unsafe("a folder on its path is a symlink".into()));
    }
    let meta = fs::symlink_metadata(path).map_err(|_| Refusal::Gone)?;
    if meta.dev() != home_device(Some(inputs.home)).map_err(refusal_of)? {
        return Err(Refusal::Locked(WorktreeStatus::OtherVolume));
    }
    if let Some(reason) = protected_reason(path, Some(inputs.home), false) {
        return Err(Refusal::Unsafe(reason));
    }
    if planned.git_dir.starts_with(path) {
        return Err(Refusal::Unsafe("it holds the repository".into()));
    }
    if !repo::owned(&planned.git_dir, path) {
        return Err(Refusal::Changed);
    }
    Ok(())
}

/// Still a real folder, the one that was listed.
fn check_folder(planned: &PlannedWorktree, path: &Path) -> Result<(), Refusal> {
    let meta = fs::symlink_metadata(path).map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => Refusal::Gone,
        _ => Refusal::Unsafe(e.to_string()),
    })?;
    if meta.file_type().is_symlink() || !meta.is_dir() {
        return Err(Refusal::Changed);
    }
    if (meta.dev(), meta.ino()) != planned.folder {
        return Err(Refusal::Changed);
    }
    Ok(())
}

/// git's entry for the worktree now, with the branch and commit that were
/// listed. Another of the repository's worktrees inside it refuses the move:
/// that one would go too.
fn listed_again(planned: &PlannedWorktree, runner: &dyn CommandRunner) -> Result<Entry, Refusal> {
    let entries = repo::worktrees(runner, &planned.git_dir)
        .ok_or_else(|| Refusal::Unsafe("git could not list the repository's worktrees".into()))?;
    if entries
        .iter()
        .any(|e| e.path != planned.path && e.path.starts_with(&planned.path))
    {
        return Err(Refusal::Unsafe("another worktree is inside it".into()));
    }
    let entry = entries
        .into_iter()
        .find(|e| e.path == planned.path)
        .ok_or(Refusal::Changed)?;
    if entry.branch != planned.branch || entry.head != planned.head {
        return Err(Refusal::Changed);
    }
    Ok(entry)
}

fn refusal_of(reason: SkipReason) -> Refusal {
    match reason {
        SkipReason::Kept { .. } | SkipReason::HoldsKept { .. } => Refusal::Kept,
        SkipReason::Missing => Refusal::Gone,
        SkipReason::Changed | SkipReason::Symlink => Refusal::Changed,
        other => Refusal::Unsafe(format!("{other:?}")),
    }
}

/// The name of the worktree's entry in its repository's `worktrees/`
/// folder, from its `.git` file; `None` when that cannot be read.
fn admin_entry(path: &Path) -> Option<String> {
    repo::pointed_git_dir(path)?
        .file_name()?
        .to_str()
        .map(str::to_string)
}

/// `git worktree prune` for the repository, so the moved worktrees' branches
/// can be checked out and deleted. git prunes every worktree whose folder is
/// missing, so its dry run is read first: when it would forget anything but
/// `moved` (another worktree on a disk that is not plugged in), nothing is
/// pruned and the others are named. Both runs are inspect-only git; a folder
/// that goes missing between them is the one window left.
fn prune(
    runner: &dyn CommandRunner,
    git_dir: &Path,
    moved: &HashSet<String>,
) -> Result<(), String> {
    let dir = git_dir
        .to_str()
        .ok_or_else(|| "its path is not valid UTF-8".to_string())?;
    let preview = git(
        runner,
        dir,
        &["worktree", "prune", "--dry-run", "--verbose"],
    )?;
    let lines: Vec<&str> = preview
        .stdout
        .lines()
        .chain(preview.stderr.lines())
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .collect();
    let known = admin_entries(git_dir, moved);
    let mut others = Vec::new();
    for line in &lines {
        match entry_of(line, &known) {
            Some(entry) if moved.contains(entry) => {}
            Some(entry) => others.push(listed_path(git_dir, entry)),
            None => return Err(format!("git's dry run said something unexpected: {line}")),
        }
    }
    if !others.is_empty() {
        return Err(would_also_forget(&others));
    }
    if lines.is_empty() {
        return Ok(());
    }
    git(runner, dir, &["worktree", "prune"]).map(|_| ())
}

fn git(
    runner: &dyn CommandRunner,
    dir: &str,
    args: &[&str],
) -> Result<crate::runner::CommandOutcome, String> {
    match run_git(runner, dir, args) {
        Ok(outcome) if outcome.success() => Ok(outcome),
        Ok(outcome) => Err(exit_detail(&outcome)),
        Err(error) => Err(error.to_string()),
    }
}

/// Every entry of the repository's `worktrees/` folder, and `moved`.
fn admin_entries(git_dir: &Path, moved: &HashSet<String>) -> Vec<String> {
    let mut entries: Vec<String> = fs::read_dir(git_dir.join("worktrees"))
        .map(|dir| {
            dir.filter_map(|e| e.ok()?.file_name().to_str().map(str::to_string))
                .collect()
        })
        .unwrap_or_default();
    entries.extend(moved.iter().cloned());
    entries
}

/// The entry a dry-run line names (`Removing worktrees/<entry>: <reason>`,
/// in any language): the longest known entry the line names that way, so an
/// entry called `a: b` is never read as `a`.
fn entry_of<'a>(line: &str, known: &'a [String]) -> Option<&'a str> {
    known
        .iter()
        .filter(|entry| line.contains(&format!("worktrees/{entry}: ")))
        .max_by_key(|entry| entry.len())
        .map(String::as_str)
}

/// The folder git recorded for `entry`, or the entry itself when that
/// cannot be read.
fn listed_path(git_dir: &Path, entry: &str) -> String {
    let admin = git_dir.join("worktrees").join(entry);
    fs::read_to_string(admin.join("gitdir"))
        .ok()
        .map(|text| {
            let dot_git = admin.join(text.trim());
            dot_git
                .parent()
                .unwrap_or(&dot_git)
                .to_string_lossy()
                .into_owned()
        })
        .unwrap_or_else(|| format!("worktrees/{entry}"))
}

/// The prune failure's detail: which other worktrees git would forget.
fn would_also_forget(others: &[String]) -> String {
    const NAMED: usize = 3;
    let mut names = others
        .iter()
        .take(NAMED)
        .cloned()
        .collect::<Vec<_>>()
        .join(", ");
    if others.len() > NAMED {
        names.push_str(&format!(" and {} more", others.len() - NAMED));
    }
    let whose = if others.len() == 1 {
        "whose folder is"
    } else {
        "whose folders are"
    };
    format!("pruning would also forget {names}, {whose} missing")
}
