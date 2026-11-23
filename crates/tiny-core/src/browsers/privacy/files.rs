//! The files a privacy write may change, read as SHA-256 digests; the
//! backup copy of them; and putting a backed-up file back in place, in
//! two steps: a checked copy beside it, then a rename.
//!
//! A file's content is `Some(digest)`, or `None` when nothing is at its
//! path. Anything at a path that is not a plain file (a symlink, a folder)
//! is an error, so nothing is ever copied through or written over a link.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use super::sha256::digest_file;

/// A file's digest; `None` when no file is there.
pub(super) type Content = Option<String>;

const DIR_MODE: u32 = 0o700;
const RESTORE_INFIX: &str = ".tinysift-restore";

static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// The digest of the plain file at `path`, `None` when nothing is there.
/// Opened without following a final symlink, and checked on the open file.
pub(super) fn content_of(path: &Path) -> Result<Content, String> {
    let file = match OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
            return Err(format!("{} is a symlink", path.display()))
        }
        Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    let meta = file
        .metadata()
        .map_err(|e| format!("could not read {}: {e}", path.display()))?;
    if !meta.file_type().is_file() {
        return Err(format!("{} is not a plain file", path.display()));
    }
    digest_file(&file)
        .map(Some)
        .map_err(|e| format!("could not read {}: {e}", path.display()))
}

/// [`content_of`] for each path, in order.
pub(super) fn contents<'a>(
    paths: impl IntoIterator<Item = &'a Path>,
) -> Result<Vec<Content>, String> {
    paths.into_iter().map(content_of).collect()
}

/// `path` with `suffix` added to its file name (`History` → `History-wal`).
pub(super) fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(suffix);
    PathBuf::from(name)
}

/// One file to keep a copy of: where it is, the name it gets in the
/// backup folder and its digest now.
pub(super) struct BackupEntry<'a> {
    pub path: &'a Path,
    pub name: &'a str,
    pub content: &'a Content,
}

/// Makes the new folder `staging` (its parent is made owner-only when
/// missing) and copies every present file into it under its backup name.
/// Each copy must hash as recorded, so the backup is the file the write
/// will edit. On failure the folder, which is this call's own, is removed.
pub(super) fn stage_backup(entries: &[BackupEntry<'_>], staging: &Path) -> Result<(), String> {
    let parent = staging
        .parent()
        .ok_or_else(|| "the backup folder has no parent".to_string())?;
    DirBuilder::new()
        .recursive(true)
        .mode(DIR_MODE)
        .create(parent)
        .map_err(|e| format!("could not make {}: {e}", parent.display()))?;
    DirBuilder::new()
        .mode(DIR_MODE)
        .create(staging)
        .map_err(|e| format!("could not make {}: {e}", staging.display()))?;
    let copied = entries.iter().try_for_each(|entry| {
        let Some(expected) = entry.content else {
            return Ok(());
        };
        let copy = staging.join(entry.name);
        fs::copy(entry.path, &copy)
            .map_err(|e| format!("could not copy {}: {e}", entry.path.display()))?;
        check(&copy, expected, "the backup copy of", entry.path)
    });
    if copied.is_err() {
        // Made just above by this call; nothing discovered is inside.
        let _ = fs::remove_dir_all(staging);
    }
    copied
}

/// Every present entry's copy in `folder` still hashes as recorded.
pub(super) fn verify_backup(entries: &[BackupEntry<'_>], folder: &Path) -> Result<(), String> {
    entries.iter().try_for_each(|entry| match entry.content {
        Some(expected) => check(
            &folder.join(entry.name),
            expected,
            "the backup of",
            entry.path,
        ),
        None => Ok(()),
    })
}

fn check(path: &Path, expected: &str, what: &str, original: &Path) -> Result<(), String> {
    match content_of(path)? {
        Some(found) if found == expected => Ok(()),
        Some(_) => Err(format!("{what} {} differs from it", original.display())),
        None => Err(format!("{what} {} is missing", original.display())),
    }
}

/// A checked copy of a backup beside the file it is to replace, not yet
/// in place. Dropping it without [`StagedRestore::commit`] removes the copy.
pub(super) struct StagedRestore {
    temp: Option<PathBuf>,
    target: PathBuf,
}

/// Copies the bytes of `backup` to a new file of our own beside `target`
/// and checks the copy hashes as `expected`. `target` is not touched; a
/// failure leaves no copy behind.
pub(super) fn stage_restore(
    backup: &Path,
    target: &Path,
    expected: &str,
) -> Result<StagedRestore, String> {
    #[cfg(test)]
    test_hooks::maybe_fail(test_hooks::Step::Stage)?;
    let (Some(dir), Some(name)) = (target.parent(), target.file_name()) else {
        return Err(format!("{} has no folder", target.display()));
    };
    match fs::symlink_metadata(dir) {
        Ok(meta) if meta.file_type().is_dir() => {}
        _ => return Err(format!("the folder of {} is gone", target.display())),
    }
    let temp = dir.join(format!(
        ".{}{RESTORE_INFIX}-{}-{}",
        name.to_string_lossy(),
        std::process::id(),
        NEXT_TEMP.fetch_add(1, Ordering::Relaxed)
    ));
    let written = write_copy(backup, &temp);
    // From here the copy (made with `create_new`, ours alone) is removed
    // on drop unless it is committed.
    let staged = StagedRestore {
        temp: Some(temp),
        target: target.to_path_buf(),
    };
    written?;
    check(staged.temp(), expected, "the restored copy of", target)?;
    Ok(staged)
}

impl StagedRestore {
    fn temp(&self) -> &Path {
        self.temp.as_deref().expect("a staged copy until committed")
    }

    /// Renames the copy over the target. A failure leaves the target as
    /// it was.
    pub(super) fn commit(mut self) -> Result<(), String> {
        #[cfg(test)]
        test_hooks::maybe_fail(test_hooks::Step::Rename)?;
        fs::rename(self.temp(), &self.target)
            .map_err(|e| format!("could not restore {}: {e}", self.target.display()))?;
        self.temp = None;
        let dir = self.target.parent().unwrap_or(Path::new("/"));
        File::open(dir)
            .and_then(|d| d.sync_all())
            .map_err(|e| format!("could not sync {}: {e}", dir.display()))
    }
}

impl Drop for StagedRestore {
    fn drop(&mut self) {
        if let Some(temp) = self.temp.take() {
            // Made by `write_copy` with `create_new`: ours alone.
            let _ = fs::remove_file(temp);
        }
    }
}

/// Copies the plain file `from` to the new file `to`, with `from`'s mode.
fn write_copy(from: &Path, to: &Path) -> Result<(), String> {
    let source = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(from)
        .map_err(|e| format!("could not read the backup {}: {e}", from.display()))?;
    let meta = source
        .metadata()
        .map_err(|e| format!("could not read the backup {}: {e}", from.display()))?;
    if !meta.file_type().is_file() {
        return Err(format!("the backup {} is not a plain file", from.display()));
    }
    let mut dest = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC)
        .open(to)
        .map_err(|e| format!("could not write {}: {e}", to.display()))?;
    let mut source = source;
    io::copy(&mut source, &mut dest)
        .and_then(|_| dest.set_permissions(fs::Permissions::from_mode(meta.permissions().mode())))
        .and_then(|()| dest.sync_all())
        .map_err(|e| format!("could not write {}: {e}", to.display()))
}

/// Only paths macOS keeps in a Trash: `~/.Trash` or a volume's `.Trashes`.
pub(super) fn in_a_trash(path: &Path) -> bool {
    path.is_absolute()
        && path.components().any(|c| {
            c == Component::Normal(".Trash".as_ref()) || c == Component::Normal(".Trashes".as_ref())
        })
}

/// A folder name from free text: path separators and control characters
/// become `-`, and the name is never `.`/`..` or empty.
pub(super) fn folder_name(text: &str) -> String {
    let cleaned: String = text
        .chars()
        .map(|c| {
            if c == '/' || c == ':' || c.is_control() {
                '-'
            } else {
                c
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches(|c: char| c == '.' || c.is_whitespace());
    if trimmed.is_empty() {
        "backup".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Fails the n-th restore step on this thread, as a full disk would.
#[cfg(test)]
pub(super) mod test_hooks {
    use std::cell::Cell;

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    pub enum Step {
        /// Writing and checking the temporary copy.
        Stage,
        /// Renaming the checked copy over the file.
        Rename,
    }

    thread_local! {
        static FAIL: Cell<Option<(Step, usize)>> = const { Cell::new(None) };
    }

    /// The `nth` (1-based) `step` on this thread fails from now on.
    pub fn fail_at(step: Step, nth: usize) {
        FAIL.with(|f| f.set(Some((step, nth))));
    }

    pub fn clear() {
        FAIL.with(|f| f.set(None));
    }

    pub(in super::super) fn maybe_fail(step: Step) -> Result<(), String> {
        FAIL.with(|f| match f.get() {
            Some((s, 1)) if s == step => {
                f.set(None);
                Err("no space left on device (injected)".into())
            }
            Some((s, n)) if s == step => {
                f.set(Some((s, n - 1)));
                Ok(())
            }
            _ => Ok(()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_symlink_or_a_folder_is_never_read_as_content() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real");
        fs::write(&real, b"x").unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&real, &link).unwrap();
        assert!(content_of(&link).is_err());
        assert!(content_of(dir.path()).is_err());
        assert_eq!(content_of(&dir.path().join("none")), Ok(None));
        assert!(content_of(&real).unwrap().is_some());
    }

    #[test]
    fn restore_puts_the_backup_bytes_in_place_and_leaves_no_temp_file() {
        let dir = tempfile::tempdir().unwrap();
        let backup = dir.path().join("backup");
        fs::write(&backup, b"original").unwrap();
        let target = dir.path().join("History");
        fs::write(&target, b"edited").unwrap();
        let expected = content_of(&backup).unwrap().unwrap();
        let staged = stage_restore(&backup, &target, &expected).unwrap();
        // Staged, not yet in place.
        assert_eq!(fs::read(&target).unwrap(), b"edited");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 3);
        staged.commit().unwrap();
        assert_eq!(fs::read(&target).unwrap(), b"original");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
        // A digest that does not match leaves the target alone.
        assert!(stage_restore(&backup, &target, "0").is_err());
        assert_eq!(fs::read(&target).unwrap(), b"original");
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
        // A staged copy dropped uncommitted leaves nothing behind.
        drop(stage_restore(&backup, &target, &expected).unwrap());
        assert_eq!(fs::read_dir(dir.path()).unwrap().count(), 2);
    }

    #[test]
    fn folder_names_have_no_separators() {
        assert_eq!(folder_name("Profiles/abc.default"), "Profiles-abc.default");
        assert_eq!(folder_name(".."), "backup");
        assert_eq!(folder_name("a:b"), "a-b");
    }

    #[test]
    fn trash_paths_are_recognised() {
        assert!(in_a_trash(Path::new("/Users/me/.Trash/x")));
        assert!(in_a_trash(Path::new("/Volumes/D/.Trashes/501/x")));
        assert!(!in_a_trash(Path::new("/Users/me/Trash/x")));
        assert!(!in_a_trash(Path::new(".Trash/x")));
    }
}
