//! Agent worktrees exported to Swift: the remembered repositories, their
//! linked worktrees with one status each, and sizes, read-only and outside
//! the gate like process queries; and the move to the Trash ([`trash`]),
//! a write that takes the gate. Swift never runs git itself.

use std::path::PathBuf;
use std::sync::{MutexGuard, PoisonError};
use std::time::{SystemTime, UNIX_EPOCH};

use tiny_core::clean::trash_plan::{account_home, trusted_home};
use tiny_core::processes::working_dir::{all_working_directories, is_running, working_directory};
use tiny_core::runner::RealRunner;
use tiny_core::worktrees::trash::PlannedWorktree;
use tiny_core::worktrees::{
    self, size, ListInputs, Listing, ManagedBy, RepoMemory, Sighting, SizeCache, SizeKey,
    WorktreeStatus,
};

use crate::{FfiError, TinySession};

mod trash;
pub use trash::*;

/// A running agent session's root process and its tool, as Swift names it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiAgentProcess {
    pub pid: u32,
    pub tool: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiManagedBy {
    Codex,
    ClaudeCode,
}

/// One status per worktree, the first that matches (spec UG-W4). Only
/// `Merged` and `LastChange` can be ticked; only `Merged` starts ticked.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum FfiWorktreeStatus {
    ActiveSession,
    Uncommitted,
    Unpushed,
    DetachedCommits,
    Managed {
        by: FfiManagedBy,
    },
    OtherVolume,
    LockedByGit {
        reason: Option<String>,
    },
    /// git could not say whether it holds work; `reason` is git's.
    NotChecked {
        reason: String,
    },
    Merged,
    LastChange {
        days: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiWorktree {
    pub path: String,
    /// The branch's short name; `None` when detached.
    pub branch: Option<String>,
    /// The full commit ID of `HEAD`.
    pub head: String,
    pub status: FfiWorktreeStatus,
    /// The tool of the last session seen in it, as Swift named it.
    pub tool: Option<String>,
    /// From the size cache when it still holds; `None` until measured.
    pub bytes: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiWorktreeRepo {
    pub name: String,
    pub git_dir: String,
    pub worktrees: Vec<FfiWorktree>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiWorktreeList {
    /// Why git cannot run (no Command Line Tools); then `repos` is empty.
    pub git_missing: Option<String>,
    /// Repositories with at least one worktree.
    pub repos: Vec<FfiWorktreeRepo>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiWorktreeSize {
    pub path: String,
    /// `None` when the folder could not be measured.
    pub bytes: Option<u64>,
}

/// The last listing's worktrees and the sizes measured so far.
#[derive(Debug, Default)]
pub(crate) struct WorktreesState {
    /// What the last listing showed: the only worktrees a move may take.
    listed: Vec<PlannedWorktree>,
    sizes: SizeCache,
    /// Tests keep the remembered list in a temporary home.
    #[cfg(test)]
    home: Option<PathBuf>,
}

#[uniffi::export]
impl TinySession {
    /// Remembers the repositories the given sessions work in, for 30 days.
    /// Reads each folder and its `.git` entries; runs no git, so it may be
    /// called whenever the sessions change.
    pub fn worktree_remember(&self, sessions: Vec<FfiAgentProcess>) -> Result<(), FfiError> {
        let home = self.worktree_home()?;
        RepoMemory::for_home(&home)
            .remember(&sightings(&sessions), now())
            .map(|_| ())
            .map_err(|e| FfiError::Io {
                detail: e.to_string(),
            })
    }

    /// Every linked worktree of the remembered repositories (these sessions'
    /// included), with its status and, where the cache still holds, its
    /// size. Runs inspect-only git, a few worktrees at a time: call it off
    /// the main thread, once when the page opens.
    pub fn worktree_list(
        &self,
        sessions: Vec<FfiAgentProcess>,
    ) -> Result<FfiWorktreeList, FfiError> {
        let home = self.worktree_home()?;
        let memory = RepoMemory::for_home(&home);
        let folders = all_working_directories();
        let sightings = sightings(&sessions);
        let listing = worktrees::list(&ListInputs {
            runner: &RealRunner,
            memory: &memory,
            home: &home,
            working_directories: &folders,
            sessions: &sightings,
            now: now(),
        });
        let repos = match listing {
            Listing::NoGit { detail } => {
                self.worktrees().listed.clear();
                return Ok(FfiWorktreeList {
                    git_missing: Some(detail),
                    repos: Vec::new(),
                });
            }
            Listing::Repos(repos) => repos,
        };
        let mut state = self.worktrees();
        state.listed = repos
            .iter()
            .flat_map(|repo| {
                repo.worktrees.iter().filter_map(|w| {
                    PlannedWorktree::of(&repo.git_dir, &w.path, w.branch.clone(), w.head.clone())
                })
            })
            .collect();
        let listed = state.listed_paths();
        state.sizes.retain(&listed);
        let repos = repos
            .into_iter()
            .filter_map(|repo| {
                let worktrees: Vec<FfiWorktree> = repo
                    .worktrees
                    .into_iter()
                    .filter_map(|w| {
                        let bytes =
                            SizeKey::of(&w.path).and_then(|k| state.sizes.lookup(&w.path, k));
                        Some(FfiWorktree {
                            path: w.path.to_str()?.to_string(),
                            branch: w.branch,
                            head: w.head,
                            status: w.status.into(),
                            tool: w.tool,
                            bytes,
                        })
                    })
                    .collect();
                (!worktrees.is_empty()).then_some(FfiWorktreeRepo {
                    name: repo.name,
                    git_dir: repo.git_dir.to_string_lossy().into_owned(),
                    worktrees,
                })
            })
            .collect();
        Ok(FfiWorktreeList {
            git_missing: None,
            repos,
        })
    }

    /// The size of every worktree of the last `worktree_list`, measuring only
    /// those whose `HEAD` or index changed since they were measured. Walks
    /// folders: call it off the main thread.
    pub fn worktree_sizes(&self) -> Vec<FfiWorktreeSize> {
        let wanted: Vec<(PathBuf, Option<SizeKey>, Option<u64>)> = {
            let state = self.worktrees();
            state
                .listed_paths()
                .into_iter()
                .map(|path| {
                    let key = SizeKey::of(&path);
                    let cached = key.and_then(|k| state.sizes.lookup(&path, k));
                    (path, key, cached)
                })
                .collect()
        };
        let missing: Vec<PathBuf> = wanted
            .iter()
            .filter(|(_, _, cached)| cached.is_none())
            .map(|(path, _, _)| path.clone())
            .collect();
        let mut measured = size::measure_all(&missing).into_iter();
        let mut state = self.worktrees();
        wanted
            .into_iter()
            .filter_map(|(path, key, cached)| {
                let bytes = cached.or_else(|| {
                    let bytes = measured.next().flatten();
                    if let (Some(key), Some(bytes)) = (key, bytes) {
                        state.sizes.store(path.clone(), key, bytes);
                    }
                    bytes
                });
                Some(FfiWorktreeSize {
                    path: path.to_str()?.to_string(),
                    bytes,
                })
            })
            .collect()
    }
}

impl WorktreesState {
    fn listed_paths(&self) -> Vec<PathBuf> {
        self.listed.iter().map(|w| w.path.clone()).collect()
    }
}

impl TinySession {
    pub(crate) fn worktrees(&self) -> MutexGuard<'_, WorktreesState> {
        self.worktrees
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    pub(crate) fn worktree_home(&self) -> Result<PathBuf, FfiError> {
        #[cfg(test)]
        if let Some(home) = self.worktrees().home.clone() {
            return Ok(home);
        }
        let env = std::env::var_os("HOME").map(PathBuf::from);
        trusted_home(env.as_deref(), account_home().as_deref())
            .map_err(|detail| FfiError::UntrustedHome { detail })
    }
}

/// Each session's folder now; `None` for a live session whose folder cannot
/// be read, nothing for one that has ended.
pub(crate) fn sightings(sessions: &[FfiAgentProcess]) -> Vec<Sighting> {
    sessions
        .iter()
        .filter_map(|session| {
            let folder = working_directory(session.pid).map(PathBuf::from);
            (folder.is_some() || is_running(session.pid)).then(|| Sighting {
                folder,
                tool: session.tool.clone(),
            })
        })
        .collect()
}

pub(crate) fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
}

impl From<WorktreeStatus> for FfiWorktreeStatus {
    fn from(status: WorktreeStatus) -> Self {
        match status {
            WorktreeStatus::ActiveSession => Self::ActiveSession,
            WorktreeStatus::Uncommitted => Self::Uncommitted,
            WorktreeStatus::Unpushed => Self::Unpushed,
            WorktreeStatus::DetachedCommits => Self::DetachedCommits,
            WorktreeStatus::Managed(by) => Self::Managed {
                by: match by {
                    ManagedBy::Codex => FfiManagedBy::Codex,
                    ManagedBy::ClaudeCode => FfiManagedBy::ClaudeCode,
                },
            },
            WorktreeStatus::OtherVolume => Self::OtherVolume,
            WorktreeStatus::LockedByGit { reason } => Self::LockedByGit { reason },
            WorktreeStatus::NotChecked { reason } => Self::NotChecked { reason },
            WorktreeStatus::Merged => Self::Merged,
            WorktreeStatus::LastChange { days } => Self::LastChange { days },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::path::Path;

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("/usr/bin/git")
            .args([
                "-c",
                "core.hooksPath=/dev/null",
                "-c",
                "commit.gpgSign=false",
            ])
            .args(["-c", "user.name=t", "-c", "user.email=t@t", "-C"])
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .unwrap();
        assert!(out.status.success(), "git {args:?}");
    }

    /// A canonical temporary folder, removed on drop.
    struct TempDir(PathBuf);

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn a_session_is_remembered_listed_and_measured_once() {
        let path = std::env::temp_dir().join(format!("tiny-ffi-worktrees-{}", std::process::id()));
        fs::create_dir_all(&path).unwrap();
        let dir = TempDir(path.canonicalize().unwrap());
        let root = dir.0.clone();
        let repo = root.join("repo");
        fs::create_dir(&repo).unwrap();
        git(&repo, &["init", "-q", "-b", "main"]);
        fs::write(repo.join("a"), vec![0u8; 32 * 1024]).unwrap();
        git(&repo, &["add", "a"]);
        git(&repo, &["commit", "-qm", "a"]);
        let wt = root.join("wt");
        git(
            &repo,
            &["worktree", "add", "-q", "-b", "feat", wt.to_str().unwrap()],
        );

        let session = TinySession::new();
        session.worktrees().home = Some(root.join("home"));
        let mut agent = std::process::Command::new("/bin/sleep")
            .arg("30")
            .current_dir(&repo)
            .spawn()
            .unwrap();
        let process = FfiAgentProcess {
            pid: agent.id(),
            tool: "claudeCode".into(),
        };
        session.worktree_remember(vec![process.clone()]).unwrap();
        let listed = session.worktree_list(vec![process]);
        agent.kill().unwrap();
        agent.wait().unwrap();

        let listed = listed.unwrap();
        assert_eq!(listed.git_missing, None);
        assert_eq!(listed.repos.len(), 1);
        let row = &listed.repos[0].worktrees[0];
        assert_eq!(row.branch.as_deref(), Some("feat"));
        assert!(matches!(row.status, FfiWorktreeStatus::LastChange { .. }));
        assert_eq!(row.bytes, None);

        let sizes = session.worktree_sizes();
        assert_eq!(sizes.len(), 1);
        let measured = sizes[0].bytes.expect("measured");
        assert!(measured >= 32 * 1024);
        // The cache answers the next listing without measuring again.
        let again = session.worktree_list(Vec::new()).unwrap();
        assert_eq!(again.repos[0].worktrees[0].bytes, Some(measured));
    }
}
