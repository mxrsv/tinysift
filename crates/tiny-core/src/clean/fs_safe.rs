//! Symlink-safe filesystem helpers.
//!
//! Uninstall's `dir_size` follows symlinks via `is_dir()` — we explicitly do
//! NOT do that here. Every metadata read uses `symlink_metadata`, walks never
//! descend into symlinked directories, and removal never calls
//! `fs::remove_dir_all`.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::freed_size::FreedBytes;
use super::scan_context::ScanContext;

/// Yields every entry under `root` without descending into symlinks. Symlink
/// entries themselves are yielded (so callers can remove them), but their
/// targets are not walked.
///
/// The walker silently skips entries whose metadata cannot be read; cleanup
/// continues on a best-effort basis.
#[allow(dead_code)]
pub fn walk_no_follow(root: &Path) -> Vec<PathBuf> {
    let mut out: Vec<PathBuf> = Vec::new();
    let meta = match fs::symlink_metadata(root) {
        Ok(m) => m,
        Err(_) => return out,
    };
    out.push(root.to_path_buf());
    if meta.file_type().is_dir() && !meta.file_type().is_symlink() {
        walk_inner(root, &mut out);
    }
    out
}

#[allow(dead_code)]
fn walk_inner(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match fs::read_dir(dir) {
        Ok(it) => it,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        out.push(path.clone());
        let ft = meta.file_type();
        if ft.is_dir() && !ft.is_symlink() {
            walk_inner(&path, out);
        }
    }
}

/// Walks `root` symlink-safe, calling `visit` for every directory entry.
/// `visit` returns whether to descend into that directory (ignored for
/// non-dir entries). Unreadable entries are skipped and recorded in `ctx`
/// under `root`; the walk stops early once `ctx` is cancelled. `root` is
/// recorded as a discovery root.
///
/// Used by providers that need conditional descent (e.g. node_modules: stop
/// recursing once a `node_modules` dir is found, so we don't flag nested
/// transitive `node_modules`).
pub fn walk_with<F>(root: &Path, ctx: &ScanContext<'_>, mut visit: F)
where
    F: FnMut(&Path, &fs::Metadata) -> bool,
{
    let meta = match fs::symlink_metadata(root) {
        Ok(m) => m,
        Err(e) => return ctx.note_unreadable(root, &e),
    };
    if meta.file_type().is_symlink() {
        return;
    }
    if !meta.file_type().is_dir() {
        return;
    }
    ctx.add_root(root);
    let descend_root = visit(root, &meta);
    if descend_root {
        walk_with_inner(root, root, ctx, &mut visit);
    }
}

fn walk_with_inner<F>(walk_root: &Path, dir: &Path, ctx: &ScanContext<'_>, visit: &mut F)
where
    F: FnMut(&Path, &fs::Metadata) -> bool,
{
    for (path, meta) in read_entries(walk_root, dir, ctx) {
        if ctx.is_cancelled() {
            return;
        }
        let ft = meta.file_type();
        if ft.is_symlink() {
            continue;
        }
        let descend = visit(&path, &meta);
        if ft.is_dir() && descend {
            walk_with_inner(walk_root, &path, ctx, visit);
        }
    }
}

/// Lists `dir`'s immediate children with their `symlink_metadata`. Read
/// failures are recorded in `ctx` under `walk_root` instead of being dropped.
fn read_entries(
    walk_root: &Path,
    dir: &Path,
    ctx: &ScanContext<'_>,
) -> Vec<(PathBuf, fs::Metadata)> {
    let entries = match fs::read_dir(dir) {
        Ok(it) => it,
        Err(e) => {
            ctx.note_unreadable(walk_root, &e);
            return Vec::new();
        }
    };
    let mut out = Vec::new();
    for entry in entries {
        let path = match entry {
            Ok(entry) => entry.path(),
            Err(e) => {
                ctx.note_unreadable(walk_root, &e);
                continue;
            }
        };
        match fs::symlink_metadata(&path) {
            Ok(meta) => out.push((path, meta)),
            Err(e) => ctx.note_unreadable(walk_root, &e),
        }
    }
    out
}

/// Immediate children of `root`, recorded as a discovery root. A missing
/// `root` yields nothing; an unreadable one is recorded in `ctx`.
pub fn list_children(root: &Path, ctx: &ScanContext<'_>) -> Vec<PathBuf> {
    let children: Vec<PathBuf> = read_entries(root, root, ctx)
        .into_iter()
        .map(|(path, _)| path)
        .collect();
    if is_dir_safe(root) {
        ctx.add_root(root);
    }
    children
}

/// Identity of a discovered path, compared again right before acting on it.
/// Device + inode catch replacement; length and mtime catch an edited file.
/// A directory's contents are not hashed: caches churn, and every cache
/// preview would otherwise go stale before the user confirms it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathFingerprint {
    device: u64,
    inode: u64,
    kind: FileKind,
    /// Length and mtime, for regular files only.
    contents: Option<(u64, Option<SystemTime>)>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FileKind {
    File,
    Dir,
    Symlink,
    Other,
}

impl PathFingerprint {
    pub fn of(meta: &fs::Metadata) -> Self {
        let ft = meta.file_type();
        let kind = if ft.is_symlink() {
            FileKind::Symlink
        } else if ft.is_dir() {
            FileKind::Dir
        } else if ft.is_file() {
            FileKind::File
        } else {
            FileKind::Other
        };
        Self {
            device: meta.dev(),
            inode: meta.ino(),
            kind,
            contents: (kind == FileKind::File).then(|| (meta.len(), meta.modified().ok())),
        }
    }
}

impl PathFingerprint {
    /// `of`, plus a directory's mtime, which changes when an entry is added
    /// or removed. Emptying the Trash removes whatever it holds then, so it
    /// must still hold what discovery measured.
    pub fn of_listing(meta: &fs::Metadata) -> Self {
        let mut fingerprint = Self::of(meta);
        if fingerprint.kind == FileKind::Dir {
            fingerprint.contents = Some((0, meta.modified().ok()));
        }
        fingerprint
    }
}

/// Fingerprint of `path` itself, never of a symlink's target.
pub fn fingerprint(path: &Path) -> std::io::Result<PathFingerprint> {
    fs::symlink_metadata(path).map(|meta| PathFingerprint::of(&meta))
}

/// True iff `path` is a real directory (not a symlink). Replacement for
/// `Path::is_dir()` which silently follows symlinks — providers must never
/// trust a symlinked target.
pub fn is_dir_safe(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .map(|m| m.file_type().is_dir())
        .unwrap_or(false)
}

/// Sums the logical length of every file under `root`, never following
/// symlinks. Returns 0 if `root` does not exist. Uninstall's measure;
/// cleanup sizes use `dir_size_checked`.
pub fn dir_size_safe(root: &Path) -> u64 {
    let mut total = 0u64;
    walk_files(root, &ScanContext::unchecked(), &mut |_, meta| {
        total = total.saturating_add(meta.len());
    });
    total
}

/// The bytes deleting `root` would free, by the [rule](super::freed_size),
/// never following symlinks. Returns 0 if `root` does not exist. Entries
/// that cannot be read are recorded in `ctx` under `root`, so the size is
/// known to be a lower bound. When the volume cannot tell which blocks other
/// files share, `root` is recorded as an upper bound. The walk stops early
/// once `ctx` is cancelled.
pub fn dir_size_checked(root: &Path, ctx: &ScanContext<'_>) -> u64 {
    freed_size(root, ctx, FreedBytes::default())
}

fn freed_size(root: &Path, ctx: &ScanContext<'_>, mut freed: FreedBytes) -> u64 {
    walk_files(root, ctx, &mut |path, meta| freed.add_file(path, meta));
    let (bytes, upper_bound, shared) = freed.finish_shared();
    if upper_bound {
        ctx.note_size_upper_bound(root);
    }
    if !shared.is_empty() {
        ctx.note_shared_blocks(root, shared);
    }
    bytes
}

/// Calls `file` for `root` itself or for every regular file under it.
fn walk_files(root: &Path, ctx: &ScanContext<'_>, file: &mut dyn FnMut(&Path, &fs::Metadata)) {
    let meta = match fs::symlink_metadata(root) {
        Ok(m) => m,
        Err(e) => return ctx.note_unreadable(root, &e),
    };
    let ft = meta.file_type();
    if ft.is_symlink() {
        return;
    }
    if ft.is_file() {
        file(root, &meta);
    } else if ft.is_dir() {
        visit_files(root, root, ctx, file);
    }
}

fn visit_files(
    walk_root: &Path,
    dir: &Path,
    ctx: &ScanContext<'_>,
    file: &mut dyn FnMut(&Path, &fs::Metadata),
) {
    for (path, meta) in read_entries(walk_root, dir, ctx) {
        if ctx.is_cancelled() {
            return;
        }
        let ft = meta.file_type();
        if ft.is_symlink() {
            continue;
        }
        if ft.is_file() {
            file(&path, &meta);
        } else if ft.is_dir() {
            visit_files(walk_root, &path, ctx, file);
        }
    }
}

/// Recursively removes `root`, never following symlinks. Files and symlink
/// entries go through `fs::remove_file`; directories are removed bottom-up
/// via `fs::remove_dir`. Never calls `fs::remove_dir_all`.
pub fn remove_recursive_safe(root: &Path) -> std::io::Result<()> {
    remove_tree(root, false)
}

/// `remove_recursive_safe` that also never enters a directory on another
/// device than `root` (a volume mounted inside it). Such a directory stays,
/// so removing `root` fails instead of reaching into another volume.
pub fn remove_recursive_one_device(root: &Path) -> std::io::Result<()> {
    remove_tree(root, true)
}

fn remove_tree(root: &Path, one_device: bool) -> std::io::Result<()> {
    let meta = match fs::symlink_metadata(root) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e),
    };
    let ft = meta.file_type();
    if ft.is_symlink() || ft.is_file() {
        return fs::remove_file(root);
    }
    if !ft.is_dir() {
        return Ok(());
    }
    remove_dir_contents(root, one_device.then_some(meta.dev()))?;
    fs::remove_dir(root)
}

fn remove_dir_contents(dir: &Path, device: Option<u64>) -> std::io::Result<()> {
    let entries = fs::read_dir(dir)?;
    for entry in entries.flatten() {
        let path = entry.path();
        let meta = match fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        let ft = meta.file_type();
        if ft.is_symlink() || ft.is_file() {
            let _ = fs::remove_file(&path);
        } else if ft.is_dir() && device.is_none_or(|dev| dev == meta.dev()) {
            remove_dir_contents(&path, device)?;
            let _ = fs::remove_dir(&path);
        }
    }
    Ok(())
}

/// The desktop's permanent deletion, a seam so tests can make it fail.
pub trait Delete: Send + Sync {
    /// Deletes `path` and everything in it for good. On `Err`, part of it
    /// may already be gone.
    fn delete(&self, path: &Path) -> std::io::Result<()>;
}

/// The real route: `remove_recursive_one_device`.
pub struct SafeDelete;

impl Delete for SafeDelete {
    fn delete(&self, path: &Path) -> std::io::Result<()> {
        remove_recursive_one_device(path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    fn tempdir(label: &str) -> PathBuf {
        let mut base = std::env::temp_dir();
        let unique = format!(
            "tiny-clean-test-{}-{}",
            label,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        base.push(unique);
        fs::create_dir_all(&base).unwrap();
        base
    }

    fn write_file(p: &Path, bytes: &[u8]) {
        let mut f = File::create(p).unwrap();
        f.write_all(bytes).unwrap();
    }

    #[test]
    fn dir_size_counts_files_recursively() {
        let dir = tempdir("size");
        write_file(&dir.join("a"), b"hello"); // 5
        fs::create_dir(dir.join("sub")).unwrap();
        write_file(&dir.join("sub/b"), b"worldworld"); // 10
        assert_eq!(dir_size_safe(&dir), 15);
        let _ = remove_recursive_safe(&dir);
    }

    #[test]
    fn dir_size_zero_for_missing_path() {
        assert_eq!(dir_size_safe(Path::new("/tmp/__tiny_clean_missing__")), 0);
    }

    #[test]
    fn remove_safe_does_not_follow_symlinks() {
        let outside = tempdir("outside");
        write_file(&outside.join("keep"), b"keep me");

        let inside = tempdir("inside");
        write_file(&inside.join("file"), b"x");
        let link = inside.join("link-to-outside");
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        assert!(outside.exists());
        assert!(link.exists());

        remove_recursive_safe(&inside).unwrap();

        assert!(!inside.exists());
        assert!(outside.exists(), "symlink target must NOT be deleted");
        assert!(outside.join("keep").exists());

        let _ = remove_recursive_safe(&outside);
    }

    #[test]
    fn permanent_deletion_removes_the_tree_and_never_follows_symlinks() {
        let outside = tempdir("delete-outside");
        write_file(&outside.join("keep"), b"keep me");
        let item = tempdir("delete-item");
        fs::create_dir(item.join("sub")).unwrap();
        write_file(&item.join("sub/file"), b"x");
        std::os::unix::fs::symlink(&outside, item.join("sub/link")).unwrap();

        SafeDelete.delete(&item).unwrap();

        assert!(fs::symlink_metadata(&item).is_err(), "the item is gone");
        assert!(outside.join("keep").exists(), "the link target stays");
        let _ = remove_recursive_safe(&outside);
    }

    #[test]
    fn dir_size_does_not_follow_symlinks() {
        let outside = tempdir("size-outside");
        write_file(&outside.join("big"), &vec![0u8; 1024]);

        let inside = tempdir("size-inside");
        write_file(&inside.join("small"), b"hi"); // 2
        let link = inside.join("link");
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        assert_eq!(dir_size_safe(&inside), 2);

        let _ = remove_recursive_safe(&inside);
        let _ = remove_recursive_safe(&outside);
    }

    #[test]
    fn checked_size_records_denied_reads_as_a_lower_bound() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir("denied");
        write_file(&dir.join("a"), b"hello"); // 5
        let locked = dir.join("locked");
        fs::create_dir(&locked).unwrap();
        write_file(&locked.join("secret"), b"0123456789");
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();

        let ctx = ScanContext::unchecked();
        let size = dir_size_checked(&dir, &ctx);
        fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(
            size,
            allocated(&dir.join("a")),
            "unreadable subtree is not counted"
        );
        let findings = ctx.take_findings();
        let note = &findings.unreadable[&dir];
        assert_eq!(note.entries, 1);
        assert!(note.first_error.to_lowercase().contains("permission"));
        let _ = remove_recursive_safe(&dir);
    }

    #[test]
    fn checked_size_of_unreadable_root_is_unavailable_not_zero() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempdir("denied-root");
        write_file(&dir.join("a"), b"hello");
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o000)).unwrap();

        let ctx = ScanContext::unchecked();
        let size = dir_size_checked(&dir, &ctx);
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o755)).unwrap();

        assert_eq!(size, 0);
        assert!(
            ctx.take_findings().unreadable.contains_key(&dir),
            "a size of 0 must be flagged as not measured"
        );
        let _ = remove_recursive_safe(&dir);
    }

    fn allocated(path: &Path) -> u64 {
        fs::symlink_metadata(path).unwrap().blocks() * 512
    }

    #[cfg(target_os = "macos")]
    fn clone_file(from: &Path, to: &Path) {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;
        let from = CString::new(from.as_os_str().as_bytes()).unwrap();
        let to = CString::new(to.as_os_str().as_bytes()).unwrap();
        // SAFETY: both arguments are valid NUL-terminated paths.
        let rc = unsafe { libc::clonefile(from.as_ptr(), to.as_ptr(), 0) };
        assert_eq!(rc, 0, "clonefile: {}", std::io::Error::last_os_error());
    }

    #[test]
    fn checked_size_counts_allocated_blocks_not_length() {
        let dir = tempdir("allocated");
        write_file(&dir.join("a"), b"hello");
        fs::create_dir(dir.join("sub")).unwrap();
        write_file(&dir.join("sub/b"), &[7u8; 10_000]);
        let expected = allocated(&dir.join("a")) + allocated(&dir.join("sub/b"));
        assert_eq!(dir_size_checked(&dir, &ScanContext::unchecked()), expected);
        let _ = remove_recursive_safe(&dir);
    }

    #[test]
    fn checked_size_counts_a_hard_link_once_and_only_when_every_link_is_inside() {
        let dir = tempdir("links");
        let item = dir.join("item");
        fs::create_dir(&item).unwrap();
        write_file(&item.join("a"), &[7u8; 50_000]);
        fs::hard_link(item.join("a"), item.join("b")).unwrap();
        let ctx = ScanContext::unchecked();
        let once = allocated(&item.join("a"));
        assert_eq!(dir_size_checked(&item, &ctx), once, "both links inside");

        fs::hard_link(item.join("a"), dir.join("outside")).unwrap();
        assert_eq!(
            dir_size_checked(&item, &ctx),
            0,
            "a link outside keeps the data"
        );
        assert_eq!(
            dir_size_checked(&item.join("a"), &ctx),
            0,
            "single-file item"
        );
        assert_eq!(
            dir_size_checked(&dir, &ctx),
            once,
            "every link inside the parent"
        );
        let _ = remove_recursive_safe(&dir);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn checked_size_counts_a_clone_family_once_and_only_when_it_is_all_inside() {
        let dir = tempdir("clones");
        let item = dir.join("item");
        fs::create_dir(&item).unwrap();
        write_file(&item.join("original"), &[7u8; 300_000]);
        clone_file(&item.join("original"), &item.join("copy"));
        let ctx = ScanContext::unchecked();
        let once = allocated(&item.join("original"));
        assert_eq!(dir_size_checked(&item, &ctx), once, "family inside");

        clone_file(&item.join("original"), &dir.join("outside"));
        assert_eq!(
            dir_size_checked(&item, &ctx),
            0,
            "a clone outside keeps the blocks"
        );
        assert_eq!(
            dir_size_checked(&item.join("copy"), &ctx),
            0,
            "single-file item"
        );
        assert_eq!(
            dir_size_checked(&dir, &ctx),
            once,
            "whole family inside the parent"
        );
        assert!(
            ctx.take_findings().size_upper_bound.is_empty(),
            "APFS answered"
        );
        let _ = remove_recursive_safe(&dir);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn a_clone_family_split_between_two_items_counts_once_in_their_union() {
        use super::super::freed_size::union_bytes;
        let dir = tempdir("split-clones");
        for item in ["a", "b"] {
            fs::create_dir(dir.join(item)).unwrap();
        }
        write_file(&dir.join("a/original"), &[7u8; 300_000]);
        clone_file(&dir.join("a/original"), &dir.join("b/copy"));
        let ctx = ScanContext::unchecked();
        assert_eq!(dir_size_checked(&dir.join("a"), &ctx), 0);
        assert_eq!(dir_size_checked(&dir.join("b"), &ctx), 0);
        let shared = ctx.take_findings().shared;
        let once = allocated(&dir.join("a/original"));
        assert_eq!(union_bytes([&shared[&dir.join("a")]]), (0, false));
        assert_eq!(union_bytes(shared.values()), (once, false));
        let _ = remove_recursive_safe(&dir);
    }

    #[test]
    fn checked_size_the_volume_cannot_explain_is_an_upper_bound() {
        use super::super::freed_size::Sharing;
        let dir = tempdir("upper-bound");
        write_file(&dir.join("a"), &[7u8; 10_000]);
        let ctx = ScanContext::unchecked();
        let size = freed_size(&dir, &ctx, FreedBytes::with_query(|_| Sharing::Unknown));
        assert_eq!(size, allocated(&dir.join("a")));
        let findings = ctx.take_findings();
        assert!(findings.size_upper_bound.contains(&dir));
        assert!(findings.unreadable.is_empty());

        let size = freed_size(&dir, &ctx, FreedBytes::with_query(|_| Sharing::Unshared));
        assert_eq!(size, allocated(&dir.join("a")));
        assert!(ctx.take_findings().size_upper_bound.is_empty());
        let _ = remove_recursive_safe(&dir);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn checked_size_needs_every_link_of_every_clone_inside() {
        let dir = tempdir("linked-clone");
        let item = dir.join("item");
        fs::create_dir(&item).unwrap();
        write_file(&item.join("original"), &[7u8; 300_000]);
        clone_file(&item.join("original"), &item.join("copy"));
        fs::hard_link(item.join("copy"), dir.join("outside")).unwrap();
        let ctx = ScanContext::unchecked();
        assert_eq!(
            dir_size_checked(&item, &ctx),
            0,
            "the outside link keeps a clone"
        );

        fs::hard_link(item.join("copy"), item.join("inside")).unwrap();
        fs::remove_file(dir.join("outside")).unwrap();
        let once = allocated(&item.join("original"));
        assert_eq!(dir_size_checked(&item, &ctx), once);
        let _ = remove_recursive_safe(&dir);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn checked_size_counts_only_the_unshared_blocks_of_an_edited_clone() {
        use std::io::{Seek, SeekFrom};
        let dir = tempdir("edited-clone");
        let item = dir.join("item");
        fs::create_dir(&item).unwrap();
        write_file(&dir.join("original"), &[7u8; 1 << 20]);
        clone_file(&dir.join("original"), &item.join("edited"));
        let mut file = fs::OpenOptions::new()
            .write(true)
            .open(item.join("edited"))
            .unwrap();
        file.seek(SeekFrom::Start(500_000)).unwrap();
        file.write_all(b"edited").unwrap();
        file.sync_all().unwrap();
        drop(file);

        let size = dir_size_checked(&item, &ScanContext::unchecked());
        assert!(size > 0, "the rewritten block is private");
        assert!(size < allocated(&item.join("edited")), "{size}");
        let _ = remove_recursive_safe(&dir);
    }

    #[test]
    fn walk_stops_when_cancelled_mid_walk() {
        use crate::clean::process::test_support::MockChecker;
        use std::sync::atomic::{AtomicBool, Ordering};
        let dir = tempdir("cancel");
        for i in 0..20 {
            fs::create_dir(dir.join(format!("d{i}"))).unwrap();
        }
        let flag = AtomicBool::new(false);
        let probe = MockChecker::none();
        let ctx = ScanContext::new(Some(&flag), &probe);
        let mut visited = 0;
        walk_with(&dir, &ctx, |_, _| {
            visited += 1;
            if visited == 3 {
                flag.store(true, Ordering::Release);
            }
            true
        });
        assert_eq!(visited, 3, "no entry is visited after cancellation");
        assert!(ctx.take_findings().roots.contains(&dir));
        let _ = remove_recursive_safe(&dir);
    }

    #[test]
    fn list_children_records_root_and_missing_root_is_silent() {
        let dir = tempdir("list");
        write_file(&dir.join("a"), b"a");
        let ctx = ScanContext::unchecked();
        assert_eq!(list_children(&dir, &ctx), vec![dir.join("a")]);
        assert!(list_children(&dir.join("missing"), &ctx).is_empty());
        let findings = ctx.take_findings();
        assert!(findings.roots.contains(&dir));
        assert!(findings.unreadable.is_empty());
        let _ = remove_recursive_safe(&dir);
    }

    #[test]
    fn fingerprint_detects_replacement_and_edits_but_not_dir_churn() {
        let dir = tempdir("fingerprint");
        let file = dir.join("f");
        write_file(&file, b"one");
        let before = fingerprint(&file).unwrap();
        assert_eq!(fingerprint(&file).unwrap(), before);
        write_file(&file, b"longer contents");
        assert_ne!(fingerprint(&file).unwrap(), before, "edited file");

        let sub = dir.join("sub");
        fs::create_dir(&sub).unwrap();
        let dir_before = fingerprint(&sub).unwrap();
        write_file(&sub.join("new"), b"x");
        assert_eq!(fingerprint(&sub).unwrap(), dir_before, "cache churn");
        // Created while `sub` still exists, so the inode cannot be reused.
        let moved = dir.join("other");
        fs::create_dir(&moved).unwrap();
        fs::remove_file(sub.join("new")).unwrap();
        fs::remove_dir(&sub).unwrap();
        fs::rename(&moved, &sub).unwrap();
        assert_ne!(fingerprint(&sub).unwrap(), dir_before, "replaced dir");

        let link = dir.join("link");
        std::os::unix::fs::symlink(&sub, &link).unwrap();
        assert_ne!(fingerprint(&link).unwrap(), fingerprint(&sub).unwrap());
        let _ = remove_recursive_safe(&dir);
    }

    #[test]
    fn walk_no_follow_skips_symlink_targets() {
        let outside = tempdir("walk-outside");
        write_file(&outside.join("hidden"), b"x");

        let inside = tempdir("walk-inside");
        write_file(&inside.join("a"), b"a");
        let link = inside.join("link");
        std::os::unix::fs::symlink(&outside, &link).unwrap();

        let walked = walk_no_follow(&inside);
        let names: Vec<String> = walked
            .iter()
            .map(|p| {
                p.file_name()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default()
            })
            .collect();
        assert!(names.iter().any(|n| n == "a"));
        assert!(names.iter().any(|n| n == "link"));
        assert!(!names.iter().any(|n| n == "hidden"));

        let _ = remove_recursive_safe(&inside);
        let _ = remove_recursive_safe(&outside);
    }
}
