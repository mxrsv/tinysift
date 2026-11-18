//! Agent worktrees: the linked git worktrees of repositories where an agent
//! session ran, each with the one status that decides whether it may go to
//! the Trash, and its size. Read-only: nothing here moves, prunes, locks or
//! writes to a repository.
//!
//! Which repositories: a session's folder is resolved to its repository's
//! git directory without running git and remembered for 30 days
//! ([`store`]), so the list survives a relaunch. Which worktrees: every
//! linked worktree git lists for that repository that the repository owns
//! ([`repo`]); a copied `.git` directory lists another repository's
//! worktrees and is never trusted with them. Every git call is inspect-only
//! (the `project_activity` runner): no hook, filter, monitor or fetch runs.
//! Status ([`status`]): one per worktree, the first that matches of the
//! table in the app's feature-upgrades spec (UG-W4). Size ([`size`]):
//! measured on request, cached by the worktree's `HEAD` and index times.

pub mod repo;
pub mod size;
pub mod status;
pub mod store;

#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use crate::runner::{CommandRunner, TOOL_TIMEOUT};

pub use repo::{locate, Location};
pub use size::{SizeCache, SizeKey};
pub use status::{ManagedBy, WorktreeStatus};
pub use store::RepoMemory;

/// Repositories are remembered this long after the last session seen in them.
pub const REMEMBER_DAYS: i64 = 30;
pub(crate) const SECS_PER_DAY: i64 = 86_400;

/// Looks for the developer folder before git is ever started: `/usr/bin/git`
/// is a shim that, without the Command Line Tools, offers to install them.
const XCODE_SELECT: &str = "/usr/bin/xcode-select";

/// A running agent session: its PID, so its folder is read now, and its tool
/// (an opaque name the caller gives, kept with the folder).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionProcess {
    pub pid: u32,
    pub tool: String,
}

/// A session whose folder is known, or `None` when the process is alive but
/// its folder cannot be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sighting {
    pub folder: Option<PathBuf>,
    pub tool: String,
}

/// One linked worktree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// As git lists it.
    pub path: PathBuf,
    /// The branch's short name; `None` when detached.
    pub branch: Option<String>,
    /// The full commit ID `HEAD` points at.
    pub head: String,
    pub status: WorktreeStatus,
    /// The tool of the last session seen in it, as the caller named it.
    pub tool: Option<String>,
}

/// A repository and its worktrees, in git's order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoWorktrees {
    /// Its git directory (`.git` of the main checkout, or a bare repository).
    pub git_dir: PathBuf,
    /// The main checkout's folder name, or the bare repository's without `.git`.
    pub name: String,
    pub worktrees: Vec<Worktree>,
}

/// What a listing found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Listing {
    /// git cannot run on this Mac (no Command Line Tools); nothing was asked.
    NoGit { detail: String },
    /// Repositories with at least one worktree; empty when there are none.
    Repos(Vec<RepoWorktrees>),
}

/// What `list` needs from the machine, so checks can supply their own.
pub struct ListInputs<'a> {
    pub runner: &'a dyn CommandRunner,
    pub memory: &'a RepoMemory,
    /// The account's home: its volume is the one with a Trash.
    pub home: &'a Path,
    /// The working directory of every process that could be read.
    pub working_directories: &'a [PathBuf],
    /// Sessions running now: their folders are remembered first, and one
    /// whose folder cannot be read locks every worktree.
    pub sessions: &'a [Sighting],
    /// Unix seconds.
    pub now: i64,
}

/// Every worktree of every remembered repository, with its status. Reads
/// only; the remembered list is written when `sessions` adds to it.
pub fn list(inputs: &ListInputs<'_>) -> Listing {
    // A write that fails leaves the list as it was: this listing still reads
    // what was remembered, and the next one tries the write again.
    let _ = inputs.memory.remember(inputs.sessions, inputs.now);
    if let Some(detail) = git_missing(inputs.runner) {
        return Listing::NoGit { detail };
    }
    let remembered = inputs.memory.recent(inputs.now);
    let unreadable_session = inputs.sessions.iter().any(|s| s.folder.is_none());
    let context = status::Context {
        runner: inputs.runner,
        home: inputs.home,
        busy: status::BusyFolders::new(inputs.working_directories),
        unreadable_session,
        now: inputs.now,
    };
    let mut repos = Vec::new();
    for git_dir in remembered.repos() {
        let Some(found) = repo::worktrees(inputs.runner, &git_dir) else {
            continue;
        };
        if found.is_empty() {
            continue;
        }
        let base = status::base_branch(inputs.runner, &git_dir);
        let statuses = status::judge_all(&context, &found, base.as_deref());
        let worktrees = found
            .into_iter()
            .zip(statuses)
            .map(|(entry, status)| Worktree {
                tool: remembered.tool_of(&entry.path),
                path: entry.path,
                branch: entry.branch,
                head: entry.head,
                status,
            })
            .collect();
        repos.push(RepoWorktrees {
            name: repo::name(&git_dir),
            git_dir,
            worktrees,
        });
    }
    Listing::Repos(repos)
}

/// Why git cannot run, or `None` when it can. Asks `xcode-select` for the
/// developer folder and looks for git in it, so the `/usr/bin/git` shim never
/// runs (and never offers an install) on a Mac without the tools.
pub fn git_missing(runner: &dyn CommandRunner) -> Option<String> {
    let outcome = match runner.output(XCODE_SELECT, &["-p"], TOOL_TIMEOUT) {
        Ok(outcome) => outcome,
        Err(error) => return Some(error.to_string()),
    };
    if !outcome.success() {
        return Some(crate::clean::providers::project_activity::exit_detail(
            &outcome,
        ));
    }
    let developer = PathBuf::from(outcome.stdout.trim());
    let git = developer.join("usr/bin/git");
    if developer.is_absolute() && runner.which(git.to_str().unwrap_or_default()) {
        None
    } else {
        Some(format!("{} has no git", developer.display()))
    }
}
