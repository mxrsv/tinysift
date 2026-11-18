//! Agent worktrees' Move to Trash for Swift. The consent contract: Swift
//! names paths, and only worktrees of the last `worktree_list` move; a path
//! that listing did not show refuses the whole call, before anything moves.
//! What moved leaves the listing, so the same call never runs twice. The
//! move is a write: it takes the session gate, and the saved "always keep"
//! rules are locked for the whole run. Core re-checks each worktree right
//! before it moves and prunes each repository after its moves
//! (`tiny_core::worktrees::trash`); Swift never runs git or `trashItem`.

use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use tiny_core::clean::finder_trash::{Trash, TrashError};
use tiny_core::clean::native_trash::NativeTrash;
use tiny_core::processes::working_dir::all_working_directories;
use tiny_core::runner::RealRunner;
use tiny_core::worktrees::repo;
use tiny_core::worktrees::trash::{
    move_to_trash, Machine, MoveInputs, MoveOutcome, MoveReport, PlannedWorktree, Refusal,
};

use super::{now, sightings, FfiAgentProcess, FfiWorktreeStatus};
use crate::clean::{protection_error, FfiStopReason};
use crate::session::CancellationToken;
use crate::{FfiError, ProgressListener, TinySession};

/// What happened to one worktree.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum FfiWorktreeMoveOutcome {
    /// In the Trash; its repository was pruned after its moves.
    Moved,
    /// It became locked since it was listed: the status it has now.
    Locked { status: FfiWorktreeStatus },
    /// Its folder is gone.
    Gone,
    /// Not the worktree that was listed (another folder, branch or commit).
    Changed,
    /// A saved "always keep" rule holds it.
    Kept,
    /// A path check refused it or could not run; English detail.
    Refused { detail: String },
    /// The Trash refused it; `still_there` is looked at afterwards.
    Failed { detail: String, still_there: bool },
    /// The run stopped before it.
    NotAttempted,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiWorktreeMoveResult {
    pub path: String,
    pub outcome: FfiWorktreeMoveOutcome,
}

/// A repository whose `git worktree prune` failed after its moves: its
/// folders are in the Trash, but git still lists them.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiWorktreePruneFailure {
    pub name: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiWorktreeMoveReport {
    /// One per path asked, in the order asked.
    pub results: Vec<FfiWorktreeMoveResult>,
    pub prune_failures: Vec<FfiWorktreePruneFailure>,
    pub stopped: Option<FfiStopReason>,
}

#[uniffi::export]
impl TinySession {
    /// Moves the given worktrees of the last `worktree_list` to the Trash
    /// through `NSFileManager` (`NativeTrash`), then runs `git worktree
    /// prune` for each repository that lost one. Each is checked again right
    /// before it moves; a row that became locked stays and says why.
    /// `sessions` are the agent sessions now, read again before each move.
    /// Progress `worktree-moved` names each path as it goes. Takes the gate:
    /// call it off the main thread, only after the user confirmed.
    pub fn worktree_move(
        &self,
        paths: Vec<String>,
        sessions: Vec<FfiAgentProcess>,
        token: Arc<CancellationToken>,
        progress: Arc<dyn ProgressListener>,
    ) -> Result<FfiWorktreeMoveReport, FfiError> {
        let machine = LiveMachine { sessions };
        self.move_worktrees(
            &paths,
            &NativeTrash::default(),
            &machine,
            &token,
            progress.as_ref(),
        )
    }
}

impl TinySession {
    /// `worktree_move` with an injected Trash and processes (the test seam).
    pub(crate) fn move_worktrees(
        &self,
        paths: &[String],
        trash: &dyn Trash,
        machine: &dyn Machine,
        token: &CancellationToken,
        progress: &dyn ProgressListener,
    ) -> Result<FfiWorktreeMoveReport, FfiError> {
        let home = self.worktree_home()?;
        let _gate = self.begin()?;
        let store = self.protection_store(&home);
        let policy = store.lease().map_err(protection_error)?;
        let plan = self.consented(paths)?;
        let on_progress = |p: tiny_core::progress::Progress| progress.on_progress(p.into());
        let report = move_to_trash(
            &plan,
            &MoveInputs {
                runner: &RealRunner,
                home: &home,
                trash,
                machine,
                policy: &policy,
                cancel: Some(token.flag()),
                progress: Some(&on_progress),
                now: now(),
            },
        );
        let moved: HashSet<PathBuf> = report
            .results
            .iter()
            .filter(|r| r.outcome == MoveOutcome::Moved)
            .map(|r| r.path.clone())
            .collect();
        let mut state = self.worktrees();
        state.listed.retain(|w| !moved.contains(&w.path));
        Ok(report.into())
    }

    /// The listed worktrees `paths` name, in their order. Nothing at all
    /// when one was not listed, is named twice, or none is named.
    fn consented(&self, paths: &[String]) -> Result<Vec<PlannedWorktree>, FfiError> {
        let invalid = |detail: &str| FfiError::PreviewInvalid {
            detail: detail.into(),
        };
        if paths.is_empty() {
            return Err(invalid("no worktree was named"));
        }
        let unique: HashSet<&String> = paths.iter().collect();
        if unique.len() != paths.len() {
            return Err(invalid("a worktree was named twice"));
        }
        let state = self.worktrees();
        paths
            .iter()
            .map(|path| {
                state
                    .listed
                    .iter()
                    .find(|w| w.path.to_str() == Some(path.as_str()))
                    .cloned()
                    .ok_or_else(|| invalid("a worktree was not in the last list; list them again"))
            })
            .collect()
    }
}

/// The processes and sessions as they are when each worktree moves.
struct LiveMachine {
    sessions: Vec<FfiAgentProcess>,
}

impl Machine for LiveMachine {
    fn working_directories(&self) -> Vec<PathBuf> {
        all_working_directories()
    }

    fn unreadable_session(&self) -> bool {
        sightings(&self.sessions).iter().any(|s| s.folder.is_none())
    }
}

impl From<MoveReport> for FfiWorktreeMoveReport {
    fn from(report: MoveReport) -> Self {
        Self {
            results: report
                .results
                .into_iter()
                .map(|r| FfiWorktreeMoveResult {
                    path: r.path.to_string_lossy().into_owned(),
                    outcome: r.outcome.into(),
                })
                .collect(),
            prune_failures: report
                .prune_failures
                .into_iter()
                .map(|f| FfiWorktreePruneFailure {
                    name: repo::name(&f.git_dir),
                    detail: f.detail,
                })
                .collect(),
            stopped: report.stopped.map(Into::into),
        }
    }
}

impl From<MoveOutcome> for FfiWorktreeMoveOutcome {
    fn from(outcome: MoveOutcome) -> Self {
        match outcome {
            MoveOutcome::Moved => Self::Moved,
            MoveOutcome::Refused(Refusal::Locked(status)) => Self::Locked {
                status: status.into(),
            },
            MoveOutcome::Refused(Refusal::Gone) => Self::Gone,
            MoveOutcome::Refused(Refusal::Changed) => Self::Changed,
            MoveOutcome::Refused(Refusal::Kept) => Self::Kept,
            MoveOutcome::Refused(Refusal::Unsafe(detail)) => Self::Refused { detail },
            MoveOutcome::Failed {
                error,
                source_present,
            } => Self::Failed {
                detail: match error {
                    TrashError::Os { detail, .. } => detail,
                    other => other.to_string(),
                },
                still_there: source_present,
            },
            MoveOutcome::NotAttempted => Self::NotAttempted,
        }
    }
}

#[cfg(test)]
mod tests;
