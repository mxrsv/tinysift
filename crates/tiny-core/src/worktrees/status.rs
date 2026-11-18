//! One status per worktree, the first of this table that matches (spec
//! UG-W4): Active session, Uncommitted, Unpushed, Detached commits, Managed
//! by a tool, On another disk, Locked by git, Merged, Last change. All but the
//! last two lock the row. Anything git cannot answer locks it too, as Not
//! checked with git's reason: when safety and convenience collide, the row
//! stays.
//!
//! The base branch is `origin/HEAD`'s branch (the local one when it exists),
//! else `main`, else `master`; none means nothing is Merged. git never
//! fetches, so a branch squash-merged on the remote reads as Last change.

use std::collections::HashSet;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::UNIX_EPOCH;

use super::repo::{normalized, pointed_git_dir, Entry};
use super::SECS_PER_DAY;
use crate::clean::providers::project_activity::{
    exit_detail, fetch_refusal, no_repository_filter, run_git,
};
use crate::runner::CommandRunner;

/// At most this many worktrees are asked about at once.
const WORKERS: usize = 4;
/// Submodules inside submodules are checked this deep; deeper is Not checked.
const SUBMODULE_DEPTH: usize = 3;
const GITLINK_MODE: &str = "160000";
const ORIGIN_HEAD: &str = "refs/remotes/origin/HEAD";
const ORIGIN_PREFIX: &str = "refs/remotes/origin/";
const BRANCH_PREFIX: &str = "refs/heads/";
const FALLBACK_BASES: [&str; 2] = ["main", "master"];
const CODEX_WORKTREES: &str = ".codex/worktrees";
const CLAUDE_WORKTREES: [&str; 2] = [".claude", "worktrees"];

/// The tool that keeps a worktree in its own folder and removes it itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ManagedBy {
    Codex,
    ClaudeCode,
}

/// A worktree's one status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorktreeStatus {
    /// A process works inside it, or a session's folder cannot be read.
    ActiveSession,
    /// Tracked or untracked changes, a changed submodule included.
    Uncommitted,
    /// Its branch has commits no other branch, remote branch or tag has.
    Unpushed,
    /// Detached, at a commit no branch, remote branch or tag has.
    DetachedCommits,
    Managed(ManagedBy),
    /// Not on the home volume, which is the one with a Trash.
    OtherVolume,
    /// `git worktree lock`, with its reason when one was given.
    LockedByGit {
        reason: Option<String>,
    },
    /// git could not say whether it holds work; `reason` is git's.
    NotChecked {
        reason: String,
    },
    /// Its branch has commits of its own, all on the base branch, and the
    /// tree is clean.
    Merged,
    /// Everything else: whole days since its last commit or index change.
    LastChange {
        days: u64,
    },
}

impl WorktreeStatus {
    /// Whether the row can never be ticked.
    pub fn is_locked(&self) -> bool {
        !matches!(self, Self::Merged | Self::LastChange { .. })
    }

    /// Whether the row starts ticked.
    pub fn ticked_by_default(&self) -> bool {
        matches!(self, Self::Merged)
    }
}

/// The folders processes work in, each with every folder above it, by
/// device and inode, so a worktree is busy whatever spelling (Unicode form,
/// case, symlink) either path uses.
#[derive(Debug, Default)]
pub(crate) struct BusyFolders {
    ids: HashSet<(u64, u64)>,
}

impl BusyFolders {
    pub(crate) fn new(working_directories: &[PathBuf]) -> Self {
        let mut ids = HashSet::new();
        let mut seen: HashSet<&Path> = HashSet::new();
        for folder in working_directories {
            for dir in folder.ancestors() {
                if !seen.insert(dir) {
                    break;
                }
                if let Ok(meta) = fs::metadata(dir) {
                    ids.insert((meta.dev(), meta.ino()));
                }
            }
        }
        Self { ids }
    }

    fn contains(&self, path: &Path) -> bool {
        fs::metadata(path).is_ok_and(|meta| self.ids.contains(&(meta.dev(), meta.ino())))
    }
}

pub(crate) struct Context<'a> {
    pub runner: &'a dyn CommandRunner,
    pub home: &'a Path,
    pub busy: BusyFolders,
    pub unreadable_session: bool,
    pub now: i64,
}

/// The status of each of `entries`, in order, asking git about up to
/// [`WORKERS`] worktrees at a time.
pub(crate) fn judge_all(
    context: &Context<'_>,
    entries: &[Entry],
    base: Option<&str>,
) -> Vec<WorktreeStatus> {
    let next = AtomicUsize::new(0);
    let results: Mutex<Vec<Option<WorktreeStatus>>> = Mutex::new(vec![None; entries.len()]);
    std::thread::scope(|scope| {
        for _ in 0..WORKERS.min(entries.len()) {
            scope.spawn(|| loop {
                let index = next.fetch_add(1, Ordering::Relaxed);
                let Some(entry) = entries.get(index) else {
                    break;
                };
                let status = judge(context, entry, base);
                results.lock().unwrap_or_else(|e| e.into_inner())[index] = Some(status);
            });
        }
    });
    results
        .into_inner()
        .unwrap_or_else(|e| e.into_inner())
        .into_iter()
        .map(|status| {
            status.unwrap_or(WorktreeStatus::NotChecked {
                reason: "it was not checked".into(),
            })
        })
        .collect()
}

/// The first row of the table that matches `entry`.
pub(crate) fn judge(context: &Context<'_>, entry: &Entry, base: Option<&str>) -> WorktreeStatus {
    if context.unreadable_session || context.busy.contains(&entry.path) {
        return WorktreeStatus::ActiveSession;
    }
    let Some(dir) = entry.path.to_str() else {
        return not_checked("its path is not valid UTF-8".into());
    };
    let runner = context.runner;
    match has_changes(runner, &entry.path, 0) {
        Err(reason) => return not_checked(reason),
        Ok(true) => return WorktreeStatus::Uncommitted,
        Ok(false) => {}
    }
    match has_own_commits(runner, dir, entry.branch.as_deref()) {
        Err(reason) => return not_checked(reason),
        Ok(true) if entry.branch.is_some() => return WorktreeStatus::Unpushed,
        Ok(true) => return WorktreeStatus::DetachedCommits,
        Ok(false) => {}
    }
    if let Some(tool) = managed_by(context.home, &entry.path) {
        return WorktreeStatus::Managed(tool);
    }
    if !on_volume_of(&entry.path, context.home) {
        return WorktreeStatus::OtherVolume;
    }
    if let Some(reason) = &entry.locked {
        let reason = Some(reason.clone()).filter(|r| !r.is_empty());
        return WorktreeStatus::LockedByGit { reason };
    }
    if let (Some(branch), Some(base)) = (entry.branch.as_deref(), base) {
        if merged(runner, dir, branch, base) {
            return WorktreeStatus::Merged;
        }
    }
    let changed = last_change(runner, dir, &entry.path).unwrap_or(0);
    let days = context.now.saturating_sub(changed).max(0) / SECS_PER_DAY;
    WorktreeStatus::LastChange {
        days: u64::try_from(days).unwrap_or(0),
    }
}

fn not_checked(reason: String) -> WorktreeStatus {
    WorktreeStatus::NotChecked { reason }
}

/// git's stdout for a call that must succeed, or why it did not.
fn ask(runner: &dyn CommandRunner, dir: &str, args: &[&str]) -> Result<String, String> {
    match run_git(runner, dir, args) {
        Ok(outcome) if outcome.success() => Ok(outcome.stdout),
        Ok(outcome) => Err(exit_detail(&outcome)),
        Err(error) => Err(error.to_string()),
    }
}

/// Whether the work tree at `path` has tracked or untracked changes, or a
/// populated submodule that moved off its recorded commit or has changes of
/// its own. `Err` when git may not be asked (a repository that could fetch
/// or defines its own filter) or does not answer.
fn has_changes(runner: &dyn CommandRunner, path: &Path, depth: usize) -> Result<bool, String> {
    let dir = path
        .to_str()
        .ok_or_else(|| "its path is not valid UTF-8".to_string())?;
    if let Some(why) = fetch_refusal(runner, dir).map_err(|e| e.to_string())? {
        return Err(why);
    }
    if !no_repository_filter(runner, dir).map_err(|e| e.to_string())? {
        return Err("its git config names a filter git status would run".into());
    }
    let status = ask(
        runner,
        dir,
        &[
            "status",
            "--porcelain",
            "-z",
            "--untracked-files=normal",
            "--ignore-submodules=all",
        ],
    )?;
    if !status.is_empty() {
        return Ok(true);
    }
    if fs::symlink_metadata(path.join(".gitmodules")).is_err() {
        return Ok(false);
    }
    if depth >= SUBMODULE_DEPTH {
        return Err("its submodules nest too deep to check".into());
    }
    let staged = ask(runner, dir, &["ls-files", "--stage", "-z"])?;
    for (commit, sub) in gitlinks(&staged) {
        let sub = path.join(sub);
        if fs::symlink_metadata(sub.join(".git")).is_err() {
            continue;
        }
        let sub_dir = sub
            .to_str()
            .ok_or_else(|| "a submodule's path is not valid UTF-8".to_string())?;
        let head = ask(
            runner,
            sub_dir,
            &["rev-parse", "--verify", "--quiet", "HEAD"],
        )?;
        if head.trim() != commit || has_changes(runner, &sub, depth + 1)? {
            return Ok(true);
        }
    }
    Ok(false)
}

/// `(commit, path)` of each submodule in `ls-files --stage -z` output.
fn gitlinks(staged: &str) -> Vec<(&str, &str)> {
    staged
        .split('\0')
        .filter_map(|line| {
            let (meta, path) = line.split_once('\t')?;
            let mut fields = meta.split(' ');
            (fields.next()? == GITLINK_MODE).then_some(())?;
            Some((fields.next()?, path))
        })
        .collect()
}

/// Whether `HEAD` has a commit no other branch, remote branch or tag has.
fn has_own_commits(
    runner: &dyn CommandRunner,
    dir: &str,
    branch: Option<&str>,
) -> Result<bool, String> {
    let exclude = branch.map(|b| format!("--exclude={b}"));
    let mut args = vec!["rev-list", "-n", "1", "HEAD", "--not"];
    args.extend(exclude.as_deref());
    args.extend(["--branches", "--remotes", "--tags", "--"]);
    Ok(!ask(runner, dir, &args)?.trim().is_empty())
}

/// The tool whose own folder holds the worktree.
fn managed_by(home: &Path, path: &Path) -> Option<ManagedBy> {
    let path = normalized(path).unwrap_or_else(|| path.to_path_buf());
    let codex = normalized(&home.join(CODEX_WORKTREES)).unwrap_or(home.join(CODEX_WORKTREES));
    if path.starts_with(&codex) {
        return Some(ManagedBy::Codex);
    }
    let names: Vec<&std::ffi::OsStr> = path
        .components()
        .filter_map(|c| match c {
            Component::Normal(name) => Some(name),
            _ => None,
        })
        .collect();
    names
        .windows(2)
        .any(|pair| pair[0] == CLAUDE_WORKTREES[0] && pair[1] == CLAUDE_WORKTREES[1])
        .then_some(ManagedBy::ClaudeCode)
}

/// Whether `path` is on the volume of `home`; false when either cannot be read.
fn on_volume_of(path: &Path, home: &Path) -> bool {
    match (fs::metadata(path), fs::metadata(home)) {
        (Ok(path), Ok(home)) => path.dev() == home.dev(),
        _ => false,
    }
}

/// `branch` has commits of its own (it moved since its reflog began) and its
/// tip is on `base`. A branch that is the base, or whose reflog git cannot
/// read, is not Merged.
fn merged(runner: &dyn CommandRunner, dir: &str, branch: &str, base: &str) -> bool {
    let reference = format!("{BRANCH_PREFIX}{branch}");
    if reference == base {
        return false;
    }
    let on_base = run_git(runner, dir, &["merge-base", "--is-ancestor", "HEAD", base]);
    if !on_base.is_ok_and(|outcome| outcome.success()) {
        return false;
    }
    let Ok(reflog) = ask(runner, dir, &["log", "-g", "--format=%H", &reference, "--"]) else {
        return false;
    };
    let Some(created) = reflog.lines().map(str::trim).rfind(|l| !l.is_empty()) else {
        return false;
    };
    let since = format!("{created}..HEAD");
    ask(runner, dir, &["rev-list", "-n", "1", &since, "--"]).is_ok_and(|own| !own.trim().is_empty())
}

/// The latest of `HEAD`'s commit time and the worktree's `HEAD` and index
/// file times, in Unix seconds.
fn last_change(runner: &dyn CommandRunner, dir: &str, path: &Path) -> Option<i64> {
    let committed = ask(runner, dir, &["log", "-1", "--format=%ct", "HEAD", "--"])
        .ok()
        .and_then(|text| text.trim().parse::<i64>().ok());
    let admin = pointed_git_dir(path);
    let touched = admin.iter().flat_map(|admin| {
        ["HEAD", "index"]
            .into_iter()
            .filter_map(move |file| modified_secs(&admin.join(file)))
    });
    committed.into_iter().chain(touched).max()
}

fn modified_secs(path: &Path) -> Option<i64> {
    let modified = fs::symlink_metadata(path).ok()?.modified().ok()?;
    i64::try_from(modified.duration_since(UNIX_EPOCH).ok()?.as_secs()).ok()
}

/// The repository's base branch as a full ref, or `None` when it has none
/// of `origin/HEAD`, `main` and `master`.
pub(crate) fn base_branch(runner: &dyn CommandRunner, git_dir: &Path) -> Option<String> {
    let dir = git_dir.to_str()?;
    let exists = |reference: &str| {
        run_git(
            runner,
            dir,
            &["rev-parse", "--verify", "--quiet", reference],
        )
        .is_ok_and(|outcome| outcome.success())
    };
    if let Ok(target) = ask(runner, dir, &["symbolic-ref", "-q", ORIGIN_HEAD]) {
        let target = target.trim();
        if let Some(name) = target.strip_prefix(ORIGIN_PREFIX) {
            let local = format!("{BRANCH_PREFIX}{name}");
            if exists(&local) {
                return Some(local);
            }
            if exists(target) {
                return Some(target.to_string());
            }
        }
    }
    FALLBACK_BASES
        .iter()
        .map(|name| format!("{BRANCH_PREFIX}{name}"))
        .find(|local| exists(local))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_merged_and_last_change_can_be_ticked_and_only_merged_starts_ticked() {
        let all = [
            WorktreeStatus::ActiveSession,
            WorktreeStatus::Uncommitted,
            WorktreeStatus::Unpushed,
            WorktreeStatus::DetachedCommits,
            WorktreeStatus::Managed(ManagedBy::Codex),
            WorktreeStatus::OtherVolume,
            WorktreeStatus::LockedByGit { reason: None },
            WorktreeStatus::NotChecked { reason: "x".into() },
            WorktreeStatus::Merged,
            WorktreeStatus::LastChange { days: 3 },
        ];
        let open: Vec<_> = all.iter().filter(|s| !s.is_locked()).collect();
        assert_eq!(
            open,
            [
                &WorktreeStatus::Merged,
                &WorktreeStatus::LastChange { days: 3 }
            ]
        );
        let ticked: Vec<_> = all.iter().filter(|s| s.ticked_by_default()).collect();
        assert_eq!(ticked, [&WorktreeStatus::Merged]);
    }

    #[test]
    fn submodules_are_read_from_the_index_by_mode() {
        let staged = ["100644 aaa 0\tREADME", "160000 bbb 0\tvendor/lib", ""].join("\0");
        assert_eq!(gitlinks(&staged), [("bbb", "vendor/lib")]);
    }

    #[test]
    fn tool_folders_are_told_apart_from_lookalikes() {
        let home = Path::new("/nonexistent-home");
        assert_eq!(
            managed_by(
                home,
                Path::new("/nonexistent-home/.codex/worktrees/a1/repo")
            ),
            Some(ManagedBy::Codex)
        );
        assert_eq!(
            managed_by(home, Path::new("/x/repo/.claude/worktrees/agent-1")),
            Some(ManagedBy::ClaudeCode)
        );
        assert_eq!(managed_by(home, Path::new("/x/.claude-worktrees/a")), None);
        assert_eq!(managed_by(home, Path::new("/x/repo/worktrees/a")), None);
    }
}
