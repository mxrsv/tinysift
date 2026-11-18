//! Which repository a folder belongs to, read from its `.git` entries without
//! running git, and the linked worktrees a repository owns.
//!
//! Ownership (UG-W3a): a worktree belongs to repository R only when its own
//! `.git` file points into R's `worktrees/` folder. A copy of a `.git`
//! directory (an agent's mirror) carries the same `worktrees/` entries and
//! lists the same folders with stale refs; those folders' `.git` files point
//! at the original, so the copy never lists, judges or prunes them.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use crate::clean::providers::project_activity::run_git;
use crate::runner::CommandRunner;

/// A `.git` file or `commondir` is one short line; anything longer is not one.
const POINTER_LIMIT: u64 = 4_096;
const GITDIR_PREFIX: &str = "gitdir:";

/// Where a session's folder sits in git.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Location {
    /// The repository's git directory: the main checkout's `.git`, or the
    /// directory a linked worktree's `commondir` names.
    pub git_dir: PathBuf,
    /// The top of the work tree holding the folder (a linked worktree, or the
    /// main checkout).
    pub top: PathBuf,
}

/// The repository holding `folder`, found by walking up to the nearest
/// `.git` entry. `None` outside git, or when a pointer cannot be followed.
/// Both paths come back `normalized`.
pub fn locate(folder: &Path) -> Option<Location> {
    let folder = fs::canonicalize(folder).ok()?;
    let top = folder
        .ancestors()
        .find(|dir| fs::symlink_metadata(dir.join(".git")).is_ok())?;
    let dot_git = top.join(".git");
    let meta = fs::symlink_metadata(&dot_git).ok()?;
    let git_dir = if meta.is_dir() {
        dot_git
    } else if meta.is_file() {
        common_dir(&pointed_git_dir(top)?)?
    } else {
        return None;
    };
    Some(Location {
        git_dir: normalized(&git_dir)?,
        top: normalized(top)?,
    })
}

/// The git directory `worktree/.git` (a file) points at, resolved. `None`
/// when `.git` is not a plain file holding one `gitdir:` line.
pub fn pointed_git_dir(worktree: &Path) -> Option<PathBuf> {
    let dot_git = worktree.join(".git");
    if !fs::symlink_metadata(&dot_git).ok()?.is_file() {
        return None;
    }
    let text = read_pointer(&dot_git)?;
    let target = text.trim().strip_prefix(GITDIR_PREFIX)?.trim();
    fs::canonicalize(worktree.join(target)).ok()
}

/// The shared git directory of a linked worktree's own `git_dir`; `git_dir`
/// itself when it names none (a main checkout with a separate git dir, or a
/// submodule).
fn common_dir(git_dir: &Path) -> Option<PathBuf> {
    let pointer = git_dir.join("commondir");
    if fs::symlink_metadata(&pointer).is_err() {
        return Some(git_dir.to_path_buf());
    }
    let text = read_pointer(&pointer)?;
    fs::canonicalize(git_dir.join(text.trim())).ok()
}

fn read_pointer(path: &Path) -> Option<String> {
    let mut text = String::new();
    File::open(path)
        .ok()?
        .take(POINTER_LIMIT)
        .read_to_string(&mut text)
        .ok()?;
    Some(text)
}

/// `path` resolved (`realpath`) and, on macOS, in Unicode's composed form, so
/// one folder has one spelling whichever way it was typed or read.
pub fn normalized(path: &Path) -> Option<PathBuf> {
    let real = fs::canonicalize(path).ok()?;
    Some(composed(real))
}

#[cfg(target_os = "macos")]
fn composed(path: PathBuf) -> PathBuf {
    use objc2_foundation::NSString;
    match path.to_str() {
        Some(text) => PathBuf::from(
            NSString::from_str(text)
                .precomposedStringWithCanonicalMapping()
                .to_string(),
        ),
        None => path,
    }
}

#[cfg(not(target_os = "macos"))]
fn composed(path: PathBuf) -> PathBuf {
    path
}

/// The repository's name: its main checkout's folder, or a bare
/// repository's folder without `.git`.
pub fn name(git_dir: &Path) -> String {
    let named = if git_dir.file_name().is_some_and(|n| n == ".git") {
        git_dir.parent().unwrap_or(git_dir)
    } else {
        git_dir
    };
    let name = named
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    name.strip_suffix(".git")
        .map(str::to_string)
        .unwrap_or(name)
}

/// One linked worktree as git lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub path: PathBuf,
    pub head: String,
    /// The branch's short name; `None` when detached.
    pub branch: Option<String>,
    /// `git worktree lock`'s reason, empty when it gave none; `None` when not locked.
    pub locked: Option<String>,
}

/// The linked worktrees `git_dir` owns: never its main checkout, never an
/// entry git calls prunable or bare, never one whose `.git` points elsewhere.
/// `None` when git does not answer.
pub fn worktrees(runner: &dyn CommandRunner, git_dir: &Path) -> Option<Vec<Entry>> {
    let dir = git_dir.to_str()?;
    let outcome = run_git(runner, dir, &["worktree", "list", "--porcelain", "-z"]).ok()?;
    if !outcome.success() {
        return None;
    }
    Some(
        parse(&outcome.stdout)
            .into_iter()
            .skip(1)
            .filter(|record| !record.prunable && !record.bare)
            .filter(|record| owned(git_dir, &record.entry.path))
            .map(|record| record.entry)
            .collect(),
    )
}

/// Whether the worktree at `path` points into `git_dir`'s own `worktrees/`.
pub fn owned(git_dir: &Path, path: &Path) -> bool {
    let (Some(own), Some(pointed)) = (normalized(git_dir), pointed_git_dir(path).map(composed))
    else {
        return false;
    };
    pointed.parent() == Some(own.join("worktrees").as_path())
}

struct Record {
    entry: Entry,
    bare: bool,
    prunable: bool,
}

/// `git worktree list --porcelain -z`: attributes end in NUL, records in an
/// empty attribute. The first record is the main checkout.
fn parse(text: &str) -> Vec<Record> {
    let mut records = Vec::new();
    let mut current: Option<Record> = None;
    for line in text.split('\0') {
        if line.is_empty() {
            records.extend(current.take());
            continue;
        }
        let (key, value) = line.split_once(' ').unwrap_or((line, ""));
        if key == "worktree" {
            records.extend(current.take());
            current = Some(Record {
                entry: Entry {
                    path: PathBuf::from(value),
                    head: String::new(),
                    branch: None,
                    locked: None,
                },
                bare: false,
                prunable: false,
            });
            continue;
        }
        let Some(record) = current.as_mut() else {
            continue;
        };
        match key {
            "HEAD" => record.entry.head = value.to_string(),
            "branch" => {
                let short = value.strip_prefix("refs/heads/").unwrap_or(value);
                record.entry.branch = Some(short.to_string());
            }
            "locked" => record.entry.locked = Some(value.to_string()),
            "bare" => record.bare = true,
            "prunable" => record.prunable = true,
            _ => {}
        }
    }
    records.extend(current);
    records
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn porcelain_records_keep_paths_with_spaces_and_every_attribute() {
        let text = "worktree /r\0HEAD aa\0branch refs/heads/main\0\0\
                    worktree /w é x\0HEAD bb\0branch refs/heads/feat/a\0locked tool x\0\0\
                    worktree /gone\0HEAD cc\0detached\0prunable gitdir file points to non-existent location\0\0\
                    worktree /bare\0bare\0\0";
        let records = parse(text);
        assert_eq!(records.len(), 4);
        let second = &records[1].entry;
        assert_eq!(second.path, PathBuf::from("/w é x"));
        assert_eq!(second.branch.as_deref(), Some("feat/a"));
        assert_eq!(second.locked.as_deref(), Some("tool x"));
        assert!(records[2].prunable && records[2].entry.branch.is_none());
        assert!(records[3].bare);
    }

    #[test]
    fn a_repository_is_named_by_its_checkout_or_its_bare_folder() {
        assert_eq!(name(Path::new("/x/tiny-cli/.git")), "tiny-cli");
        assert_eq!(name(Path::new("/x/mirror.git")), "mirror");
        assert_eq!(name(Path::new("/x/plain")), "plain");
    }
}
