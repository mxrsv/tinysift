//! The repositories where an agent session ran, kept [`REMEMBER_DAYS`] after
//! the last session seen in one, so Agent worktrees lists them after a
//! relaunch or once the session has ended.
//!
//! One owner-only JSON file, `~/Library/Application Support/Tiny/agent-repos.json`,
//! beside Clean's protection store: core decides which repositories are
//! candidates (the app never runs git), so core keeps the list. It holds each
//! repository's git directory with the time it was last seen, and each
//! session folder's work tree with the tool that ran there (the row's logo).
//! It is not policy: a missing, unreadable or malformed file reads as empty,
//! which can only list fewer worktrees, and a write replaces it whole through
//! a synced temporary file of its own and a rename. A change reads, edits and
//! writes the file under an exclusive `flock` on `agent-repos.json.lock`, so
//! two callers at once (the app passing sessions while a listing runs) never
//! drop each other's entries.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use super::repo::{locate, normalized};
use super::{Sighting, REMEMBER_DAYS, SECS_PER_DAY};

const HOME_RELATIVE: &str = "Library/Application Support/Tiny";
const FILE_NAME: &str = "agent-repos.json";
const TEMP_SUFFIX: &str = ".tmp";
const LOCK_SUFFIX: &str = ".lock";
const VERSION: u32 = 1;
const MAX_BYTES: u64 = 1 << 20;
/// A sighting this soon after the last one changes nothing on disk, so a
/// session seen at every sample writes at most once an hour.
const REFRESH_SECS: i64 = 3_600;
/// Bounds on the file; the oldest entries go first.
const MAX_REPOS: usize = 1_000;
const MAX_FOLDERS: usize = 4_000;
const DIR_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;

#[derive(Debug, Default, Clone, PartialEq, Serialize, Deserialize)]
struct Document {
    version: u32,
    repos: Vec<RepoEntry>,
    folders: Vec<FolderEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct RepoEntry {
    git_dir: String,
    last_seen: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct FolderEntry {
    /// The top of the work tree the session ran in.
    path: String,
    tool: String,
    last_seen: i64,
}

/// The remembered list on disk.
#[derive(Debug, Clone)]
pub struct RepoMemory {
    file: PathBuf,
}

/// What was remembered within the last [`REMEMBER_DAYS`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Remembered {
    repos: Vec<PathBuf>,
    folders: Vec<(String, String)>,
}

impl Remembered {
    /// Git directories, most recently seen first.
    pub fn repos(&self) -> Vec<PathBuf> {
        self.repos.clone()
    }

    /// The tool of the last session seen in the work tree at `path`.
    pub fn tool_of(&self, path: &Path) -> Option<String> {
        let key = normalized(path)?;
        let key = key.to_str()?;
        self.folders
            .iter()
            .find(|(folder, _)| folder == key)
            .map(|(_, tool)| tool.clone())
    }
}

impl RepoMemory {
    pub fn at(file: PathBuf) -> Self {
        Self { file }
    }

    /// The list for the account whose home is `home`.
    pub fn for_home(home: &Path) -> Self {
        Self::at(home.join(HOME_RELATIVE).join(FILE_NAME))
    }

    pub fn file(&self) -> &Path {
        &self.file
    }

    /// Adds the repositories and work trees of the sessions whose folder is
    /// known, and forgets entries older than [`REMEMBER_DAYS`]. Runs no git:
    /// each folder is resolved through its `.git` entries. Writes only when
    /// something changed; returns whether it wrote.
    pub fn remember(&self, sessions: &[Sighting], now: i64) -> io::Result<bool> {
        let seen: Vec<(String, String, &str)> = sessions
            .iter()
            .filter_map(|sighting| {
                let location = locate(sighting.folder.as_deref()?)?;
                Some((
                    location.git_dir.to_str()?.to_string(),
                    location.top.to_str()?.to_string(),
                    sighting.tool.as_str(),
                ))
            })
            .collect();
        // Without the lock first: a sample that changes nothing creates no
        // file, not even the lock.
        if updated(&self.read(), &seen, now).is_none() {
            return Ok(false);
        }
        let _lock = self.lock()?;
        let Some(document) = updated(&self.read(), &seen, now) else {
            return Ok(false);
        };
        self.write(&document)?;
        Ok(true)
    }

    /// The entries seen within the last [`REMEMBER_DAYS`] of `now`.
    pub fn recent(&self, now: i64) -> Remembered {
        let document = fresh(&self.read(), now);
        let mut repos = document.repos;
        repos.sort_by(|a, b| {
            b.last_seen
                .cmp(&a.last_seen)
                .then(a.git_dir.cmp(&b.git_dir))
        });
        Remembered {
            repos: repos
                .into_iter()
                .map(|r| PathBuf::from(r.git_dir))
                .collect(),
            folders: document
                .folders
                .into_iter()
                .map(|f| (f.path, f.tool))
                .collect(),
        }
    }

    fn read(&self) -> Document {
        let Ok(file) = File::open(&self.file) else {
            return Document::default();
        };
        let mut bytes = Vec::new();
        if file.take(MAX_BYTES + 1).read_to_end(&mut bytes).is_err()
            || bytes.len() as u64 > MAX_BYTES
        {
            return Document::default();
        }
        match serde_json::from_slice::<Document>(&bytes) {
            Ok(document) if document.version == VERSION => document,
            _ => Document::default(),
        }
    }

    /// The folder, made owner-only when missing.
    fn dir(&self) -> io::Result<&Path> {
        let dir = self
            .file
            .parent()
            .ok_or_else(|| io::Error::other("the list has no folder"))?;
        DirBuilder::new()
            .recursive(true)
            .mode(DIR_MODE)
            .create(dir)?;
        Ok(dir)
    }

    /// `path` with `suffix` added to its name.
    fn sibling(&self, suffix: &str) -> PathBuf {
        let mut name = self.file.as_os_str().to_os_string();
        name.push(suffix);
        PathBuf::from(name)
    }

    /// The exclusive lock for a read, edit and write; held until dropped.
    fn lock(&self) -> io::Result<File> {
        self.dir()?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(FILE_MODE)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(self.sibling(LOCK_SUFFIX))?;
        file.lock()?;
        Ok(file)
    }

    fn write(&self, document: &Document) -> io::Result<()> {
        self.dir()?;
        // A temporary file of this write's own: a writer that does not hold
        // the lock can only lose its own change, never tear the file.
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let temp = self.sibling(&format!(
            ".{}.{}{TEMP_SUFFIX}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let bytes = serde_json::to_vec_pretty(document).map_err(io::Error::other)?;
        let written = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(FILE_MODE)
            .open(&temp)
            .and_then(|mut file| {
                file.write_all(&bytes)?;
                file.sync_all()
            })
            .and_then(|()| fs::rename(&temp, &self.file));
        if written.is_err() {
            let _ = fs::remove_file(&temp);
        }
        written
    }
}

/// `before` with the sessions `seen` (git directory, work tree, tool) added
/// and old entries forgotten; `None` when that changes nothing.
fn updated(before: &Document, seen: &[(String, String, &str)], now: i64) -> Option<Document> {
    let mut document = fresh(before, now);
    for (git_dir, top, tool) in seen {
        upsert_repo(&mut document.repos, git_dir, now);
        upsert_folder(&mut document.folders, top, tool, now);
    }
    bound(&mut document);
    let same = document.repos == before.repos && document.folders == before.folders;
    (!same).then_some(document)
}

/// `document` without entries older than [`REMEMBER_DAYS`] at `now`.
fn fresh(document: &Document, now: i64) -> Document {
    let kept = |seen: i64| now.saturating_sub(seen) <= REMEMBER_DAYS * SECS_PER_DAY;
    Document {
        version: VERSION,
        repos: document
            .repos
            .iter()
            .filter(|r| kept(r.last_seen))
            .cloned()
            .collect(),
        folders: document
            .folders
            .iter()
            .filter(|f| kept(f.last_seen))
            .cloned()
            .collect(),
    }
}

fn upsert_repo(repos: &mut Vec<RepoEntry>, git_dir: &str, now: i64) {
    match repos.iter_mut().find(|r| r.git_dir == git_dir) {
        Some(entry) if now - entry.last_seen >= REFRESH_SECS => entry.last_seen = now,
        Some(_) => {}
        None => repos.push(RepoEntry {
            git_dir: git_dir.to_string(),
            last_seen: now,
        }),
    }
}

fn upsert_folder(folders: &mut Vec<FolderEntry>, path: &str, tool: &str, now: i64) {
    match folders.iter_mut().find(|f| f.path == path) {
        Some(entry) if entry.tool != tool || now - entry.last_seen >= REFRESH_SECS => {
            entry.tool = tool.to_string();
            entry.last_seen = now;
        }
        Some(_) => {}
        None => folders.push(FolderEntry {
            path: path.to_string(),
            tool: tool.to_string(),
            last_seen: now,
        }),
    }
}

/// Keeps the newest entries within the file's bounds.
fn bound(document: &mut Document) {
    if document.repos.len() > MAX_REPOS {
        document
            .repos
            .sort_by_key(|r| std::cmp::Reverse(r.last_seen));
        document.repos.truncate(MAX_REPOS);
    }
    if document.folders.len() > MAX_FOLDERS {
        document
            .folders
            .sort_by_key(|f| std::cmp::Reverse(f.last_seen));
        document.folders.truncate(MAX_FOLDERS);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_missing_or_malformed_file_reads_as_empty() {
        let dir = tempfile::tempdir().unwrap();
        let memory = RepoMemory::at(dir.path().join("agent-repos.json"));
        assert_eq!(memory.recent(0), Remembered::default());
        fs::write(memory.file(), b"{not json").unwrap();
        assert_eq!(memory.recent(0), Remembered::default());
        fs::write(memory.file(), br#"{"version":9,"repos":[],"folders":[]}"#).unwrap();
        assert_eq!(memory.recent(0), Remembered::default());
    }

    #[test]
    fn a_folder_outside_git_is_not_remembered_and_writes_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let memory = RepoMemory::at(dir.path().join("list/agent-repos.json"));
        let plain = dir.path().join("plain");
        fs::create_dir(&plain).unwrap();
        let sightings = [
            Sighting {
                folder: Some(plain),
                tool: "codex".into(),
            },
            Sighting {
                folder: None,
                tool: "codex".into(),
            },
        ];
        assert!(!memory.remember(&sightings, 100).unwrap());
        assert!(!memory.file().exists());
        assert!(!memory.file().parent().unwrap().exists(), "nor its lock");
    }

    /// Many callers at once, each adding its own repositories, as the app's
    /// `worktree_remember` and a listing do: every one is kept and the file
    /// always reads.
    #[test]
    fn writers_at_once_keep_every_repository() {
        const WRITERS: usize = 8;
        const EACH: usize = 12;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let file = root.join("state/agent-repos.json");
        std::thread::scope(|scope| {
            for writer in 0..WRITERS {
                let (root, file) = (&root, &file);
                scope.spawn(move || {
                    let memory = RepoMemory::at(file.clone());
                    for n in 0..EACH {
                        let repo = root.join(format!("repo-{writer}-{n}"));
                        fs::create_dir_all(repo.join(".git")).unwrap();
                        let sighting = Sighting {
                            folder: Some(repo),
                            tool: "codex".into(),
                        };
                        assert!(memory.remember(&[sighting], 1_000).unwrap());
                        assert!(!memory.recent(1_000).repos().is_empty(), "the file reads");
                    }
                });
            }
        });
        let remembered = RepoMemory::at(file.clone()).recent(1_000).repos();
        assert_eq!(remembered.len(), WRITERS * EACH);
        let leftovers: Vec<_> = fs::read_dir(file.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok()?.file_name().into_string().ok())
            .filter(|name| name.ends_with(TEMP_SUFFIX))
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }
}
