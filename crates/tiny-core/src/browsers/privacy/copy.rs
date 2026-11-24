//! A private copy of one SQLite database, opened read-only.
//!
//! The original and its `-journal`, `-wal` and `-shm` files are copied
//! into a fresh folder under the temporary directory, readable by this
//! user only, and only the copy is opened. The folder is removed when the
//! [`Snapshot`] is dropped. A copy that holds a hot rollback journal is
//! first opened read-write, once, so SQLite rolls the copy back to its
//! last committed state; the original is never opened.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, ErrorCode, OpenFlags};

use super::{PrivacyIssue, PrivacyIssueKind};

/// The files SQLite keeps beside a database.
const SIBLING_SUFFIXES: [&str; 3] = ["-journal", "-wal", "-shm"];

const DIR_PREFIX: &str = "tinysift-privacy";

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

/// An open read-only connection on a private copy.
pub(super) struct Snapshot {
    // Declared first so the connection closes before the folder goes.
    connection: Connection,
    _dir: TempDir,
}

impl Snapshot {
    /// Copies `original` with its siblings and opens the copy read-only.
    pub fn of(original: &Path) -> Result<Self, PrivacyIssue> {
        let file_name = original
            .file_name()
            .ok_or_else(|| unreadable(original, "the path has no file name"))?;
        let dir = TempDir::create()
            .map_err(|e| io_issue(original, "could not make a temporary folder", &e))?;
        let copy = dir.path.join(file_name);
        copy_regular_file(original, &copy)?
            .ok_or_else(|| unreadable(original, "the database is gone"))?;
        for suffix in SIBLING_SUFFIXES {
            let mut name = file_name.to_os_string();
            name.push(suffix);
            copy_regular_file(&original.with_file_name(&name), &dir.path.join(&name))?;
        }
        let connection = open_copy(&copy).map_err(|e| sqlite_issue(original, &e))?;
        Ok(Snapshot {
            connection,
            _dir: dir,
        })
    }

    pub fn connection(&self) -> &Connection {
        &self.connection
    }
}

/// Copies a regular file; `Ok(None)` when it does not exist. A symlink or
/// anything else that is not a plain file is refused.
fn copy_regular_file(from: &Path, to: &Path) -> Result<Option<()>, PrivacyIssue> {
    let meta = match from.symlink_metadata() {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_issue(from, "could not read the file", &e)),
    };
    if !meta.file_type().is_file() {
        return Err(unreadable(from, "not a regular file"));
    }
    fs::copy(from, to).map_err(|e| io_issue(from, "could not copy the file", &e))?;
    Ok(Some(()))
}

fn open_copy(copy: &Path) -> rusqlite::Result<Connection> {
    let read_only = OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let connection = Connection::open_with_flags(copy, read_only)?;
    match probe(&connection) {
        Ok(()) => Ok(connection),
        Err(e) if e.sqlite_error_code() == Some(ErrorCode::ReadOnly) => {
            // A hot journal needs a rollback, which a read-only handle
            // cannot do. Let SQLite do it on the copy, then read again.
            drop(connection);
            let writable = OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX;
            probe(&Connection::open_with_flags(copy, writable)?)?;
            let connection = Connection::open_with_flags(copy, read_only)?;
            probe(&connection)?;
            Ok(connection)
        }
        Err(e) => Err(e),
    }
}

/// Reads the schema, which is when SQLite checks the file and its journal.
fn probe(connection: &Connection) -> rusqlite::Result<()> {
    connection.query_row("SELECT count(*) FROM sqlite_master", [], |_| Ok(()))
}

fn unreadable(path: &Path, detail: &str) -> PrivacyIssue {
    PrivacyIssue::new(PrivacyIssueKind::Unreadable, path, detail)
}

fn io_issue(path: &Path, what: &str, error: &io::Error) -> PrivacyIssue {
    let kind = if error.kind() == io::ErrorKind::PermissionDenied {
        PrivacyIssueKind::PermissionDenied
    } else {
        PrivacyIssueKind::Unreadable
    };
    PrivacyIssue::new(kind, path, format!("{what}: {error}"))
}

pub(super) fn sqlite_issue(path: &Path, error: &rusqlite::Error) -> PrivacyIssue {
    PrivacyIssue::new(
        PrivacyIssueKind::Unreadable,
        path,
        format!("not a readable database: {error}"),
    )
}

/// A folder of this process's own, removed on drop.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn create() -> io::Result<Self> {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.subsec_nanos());
        loop {
            let n = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("{DIR_PREFIX}-{}-{nanos}-{n}", std::process::id()));
            match private_dir_builder().create(&path) {
                Ok(()) => return Ok(TempDir { path }),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e),
            }
        }
    }
}

/// Cookie copies are private: the folder is the user's alone.
fn private_dir_builder() -> fs::DirBuilder {
    let mut builder = fs::DirBuilder::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder
}

impl Drop for TempDir {
    fn drop(&mut self) {
        // Our own folder, made above; nothing discovered is removed here.
        let _ = fs::remove_dir_all(&self.path);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_dir_is_private_and_removed_on_drop() {
        let dir = TempDir::create().unwrap();
        let path = dir.path.clone();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o700);
        }
        fs::write(path.join("Cookies"), b"x").unwrap();
        drop(dir);
        assert!(!path.exists());
    }

    #[test]
    fn a_symlinked_database_is_refused() {
        let home = tempfile::tempdir().unwrap();
        let real = home.path().join("real");
        fs::write(&real, b"x").unwrap();
        let link = home.path().join("History");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        let issue = Snapshot::of(&link).err().unwrap();
        assert_eq!(issue.kind, PrivacyIssueKind::Unreadable);
    }
}
