//! The bytes removing one path would free on its own: its private allocated
//! bytes. Blocks shared with an APFS clone do not count, and a hard-linked
//! inode counts once, and only when every one of its links lies inside the
//! measured path: a link left outside keeps the blocks, so removing this
//! path frees nothing for them (the rule `freed_size` applies to cleanup).
//!
//! This answers for a single path. [`super::freed_size`] owns the rule for
//! cleanup items, which also credits clone families removed together.
//!
//! - A file that shares nothing counts its allocated blocks.
//! - A partly shared file counts `ATTR_CMNEXT_PRIVATESIZE`, never more than
//!   its allocated blocks.
//! - A full clone counts 0: another file still holds every block.
//! - When the volume cannot answer (not APFS, a failed query, not macOS) the
//!   allocated blocks count and the figure is marked as a fallback.
//! - A folder is walked with `symlink_metadata`; symlinks are never followed.

use super::freed_size::{sharing_of, Sharing};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

/// Private bytes of a path and whether the volume vouched for them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PrivateSize {
    pub bytes: u64,
    /// False when some file fell back to its allocated blocks (or could not
    /// be read), so `bytes` is an upper bound.
    pub exact: bool,
}

/// Private allocated bytes of the file, or of every file under the folder.
/// Only the root failing to read is an error; an unreadable entry below it
/// makes the figure inexact.
pub fn private_allocated_bytes(path: &Path) -> io::Result<PrivateSize> {
    let root = fs::symlink_metadata(path)?;
    let mut total = PrivateSize {
        bytes: 0,
        exact: true,
    };
    // Per inode: links met inside the path, its link count, and what it counts.
    let mut linked: HashMap<(u64, u64), (u64, u64, u64)> = HashMap::new();
    let mut pending: Vec<(PathBuf, fs::Metadata)> = vec![(path.to_path_buf(), root)];
    while let Some((current, meta)) = pending.pop() {
        let kind = meta.file_type();
        if kind.is_dir() {
            match fs::read_dir(&current) {
                Ok(entries) => {
                    for entry in entries {
                        match entry.and_then(|e| e.metadata().map(|m| (e.path(), m))) {
                            Ok(child) => pending.push(child),
                            Err(_) => total.exact = false,
                        }
                    }
                }
                Err(_) => total.exact = false,
            }
        } else if kind.is_file() {
            let key = (meta.dev(), meta.ino());
            if let Some(entry) = linked.get_mut(&key) {
                entry.0 += 1;
                continue;
            }
            let allocated = meta.blocks().saturating_mul(512);
            let (bytes, exact) = match sharing_of(&current) {
                Sharing::Unshared => (allocated, true),
                Sharing::Partial { private } => (private.min(allocated), true),
                Sharing::Clone { .. } => (0, true),
                Sharing::Unknown => (allocated, false),
            };
            total.exact &= exact;
            linked.insert(key, (1, meta.nlink(), bytes));
        }
    }
    for (seen, nlink, bytes) in linked.into_values() {
        if seen >= nlink {
            total.bytes = total.bytes.saturating_add(bytes);
        }
    }
    Ok(total)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const SIZE: usize = 1 << 20;

    fn write(path: &Path, bytes: usize) {
        let mut file = fs::File::create(path).unwrap();
        // Non-zero, so nothing compresses or stays sparse.
        file.write_all(&vec![0xA5; bytes]).unwrap();
        file.sync_all().unwrap();
    }

    fn allocated(path: &Path) -> u64 {
        fs::symlink_metadata(path).unwrap().blocks() * 512
    }

    #[cfg(target_os = "macos")]
    fn clone_file(from: &Path, to: &Path) -> bool {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let from = CString::new(from.as_os_str().as_bytes()).unwrap();
        let to = CString::new(to.as_os_str().as_bytes()).unwrap();
        // SAFETY: both are NUL-terminated paths that outlive the call.
        unsafe { libc::clonefile(from.as_ptr(), to.as_ptr(), 0) == 0 }
    }

    #[test]
    fn a_plain_file_counts_its_allocated_blocks() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("plain");
        write(&file, SIZE);
        let size = private_allocated_bytes(&file).unwrap();
        assert_eq!(size.bytes, allocated(&file));
        #[cfg(target_os = "macos")]
        assert!(size.exact);
    }

    #[test]
    fn a_missing_path_is_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(private_allocated_bytes(&dir.path().join("missing")).is_err());
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_clone_and_its_original_free_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        write(&a, SIZE);
        if !clone_file(&a, &b) {
            eprintln!("skipped: the temp dir is not on APFS");
            return;
        }
        for path in [&a, &b] {
            let size = private_allocated_bytes(path).unwrap();
            assert_eq!(size.bytes, 0, "{path:?}");
            assert!(size.exact);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_partly_modified_clone_frees_only_its_own_blocks() {
        use std::io::{Seek, SeekFrom};
        let dir = tempfile::tempdir().unwrap();
        let (a, b) = (dir.path().join("a"), dir.path().join("b"));
        write(&a, 8 * SIZE);
        if !clone_file(&a, &b) {
            eprintln!("skipped: the temp dir is not on APFS");
            return;
        }
        let mut file = fs::OpenOptions::new().write(true).open(&b).unwrap();
        file.seek(SeekFrom::Start(0)).unwrap();
        file.write_all(&vec![0x5A; SIZE]).unwrap();
        file.sync_all().unwrap();
        let size = private_allocated_bytes(&b).unwrap();
        assert!(size.bytes > 0, "{size:?}");
        assert!(size.bytes < allocated(&b), "{size:?}");
    }

    #[test]
    fn a_hard_linked_file_counts_once_in_a_folder() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("one");
        write(&file, SIZE);
        fs::hard_link(&file, dir.path().join("two")).unwrap();
        let size = private_allocated_bytes(dir.path()).unwrap();
        assert_eq!(size.bytes, allocated(&file));
    }

    #[test]
    fn a_hard_link_with_its_twin_outside_the_path_frees_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let (inside, outside) = (dir.path().join("in"), dir.path().join("out"));
        fs::create_dir(&inside).unwrap();
        fs::create_dir(&outside).unwrap();
        write(&inside.join("one"), SIZE);
        fs::hard_link(inside.join("one"), outside.join("two")).unwrap();
        assert_eq!(
            private_allocated_bytes(&inside.join("one")).unwrap().bytes,
            0
        );
        assert_eq!(private_allocated_bytes(&inside).unwrap().bytes, 0);
        // Measured from the folder that holds both links, it counts once.
        let both = private_allocated_bytes(dir.path()).unwrap();
        assert_eq!(both.bytes, allocated(&inside.join("one")));
    }

    #[test]
    fn a_symlink_is_not_followed() {
        let outside = tempfile::tempdir().unwrap();
        write(&outside.path().join("big"), 4 * SIZE);
        let dir = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("link")).unwrap();
        let size = private_allocated_bytes(dir.path()).unwrap();
        assert_eq!(size.bytes, 0);
    }
}
