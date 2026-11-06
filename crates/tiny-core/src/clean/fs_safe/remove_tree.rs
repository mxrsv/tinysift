//! Descriptor-relative removal. No directory pathname is reopened after its
//! descriptor is acquired. Entry replacement is refused when detectable;
//! unlinkat itself cannot compare an expected inode atomically.

use std::ffi::{CStr, CString, OsStr};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::path::{Component, Path, PathBuf};
use std::ptr::NonNull;

const DIRECTORY_FLAGS: libc::c_int = libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC;
// Ancestors are searched, never enumerated: requiring read would refuse
// otherwise accessible children of an execute-only directory.
#[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
const SEARCH_ACCESS: libc::c_int = libc::O_SEARCH;
#[cfg(any(target_os = "linux", target_os = "android"))]
const SEARCH_ACCESS: libc::c_int = libc::O_PATH;

#[derive(Clone, Copy)]
enum Stage {
    BeforeAncestorOpen,
    BeforeDirectoryOpen,
    AfterDirectoryOpen,
    BeforeUnlink,
}

// Private synchronization seam; production supplies only a no-op observer.
type Observer<'a> = &'a mut dyn FnMut(Stage, &Path);

/// The first failures a best-effort removal keeps; the rest are only counted.
const KEPT_FAILURES: usize = 16;

/// What a best-effort removal ([`remove_each`]) did. Every guard of the
/// fail-fast removal still holds per entry: an entry that fails one stays,
/// and its siblings are still removed.
#[derive(Debug, Default)]
pub struct TreeRemoval {
    /// The first failures, in walk order (at most 16).
    pub failures: Vec<EntryFailure>,
    /// Every entry that could not be removed, past `failures` too. A folder
    /// left only because something inside it stayed is not counted.
    pub failed_entries: u64,
    /// Entries unlinked, folders included.
    pub removed_entries: u64,
}

impl TreeRemoval {
    /// Nothing failed: the whole tree is gone (or was already).
    pub fn is_complete(&self) -> bool {
        self.failed_entries == 0
    }

    /// One failure at `path`, nothing removed: what a route that cannot
    /// continue past an entry reports.
    pub fn failed_at(path: &Path, error: io::Error) -> Self {
        let mut removal = Self::default();
        removal.record(path, error);
        removal
    }

    fn record(&mut self, path: &Path, error: io::Error) {
        self.failed_entries += 1;
        if self.failures.len() < KEPT_FAILURES {
            self.failures.push(EntryFailure {
                path: path.to_path_buf(),
                error,
            });
        }
    }
}

/// One entry a best-effort removal left, with the error that kept it.
#[derive(Debug)]
pub struct EntryFailure {
    pub path: PathBuf,
    pub error: io::Error,
}

/// The error for an entry replaced or edited during deletion, which is
/// refused (left as it is).
#[derive(Debug)]
struct EntryChanged;

impl std::fmt::Display for EntryChanged {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("directory entry changed during deletion")
    }
}

impl std::error::Error for EntryChanged {}

/// True when `error` refused an entry replaced during deletion.
pub fn is_entry_changed(error: &io::Error) -> bool {
    error
        .get_ref()
        .is_some_and(|inner| inner.is::<EntryChanged>())
}

/// Fail fast stops at the first entry that fails; best effort records it
/// and goes on with the rest of the tree.
struct Sink {
    best_effort: bool,
    removal: TreeRemoval,
}

impl Sink {
    fn fail(&mut self, path: &Path, error: io::Error) -> io::Result<()> {
        if !self.best_effort {
            return Err(error);
        }
        self.removal.record(path, error);
        Ok(())
    }
}

/// Which file a removal may start from: the device and inode discovery
/// fingerprinted. A root that is another file now is refused whole.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    pub device: u64,
    pub inode: u64,
}

impl FileIdentity {
    // `st_dev` and `st_ino` have other widths on other platforms.
    #[allow(clippy::unnecessary_cast)]
    fn matches(self, stat: &libc::stat) -> bool {
        // As `MetadataExt::dev` and `ino` widen them.
        self.device == stat.st_dev as u64 && self.inode == stat.st_ino as u64
    }
}

pub(super) fn remove(root: &Path, one_device: bool) -> io::Result<()> {
    remove_observed(root, one_device, &mut |_, _| {})
}

/// `remove`, but an entry that fails stays and the rest of the tree is
/// still removed. Failures above `root`'s children (its ancestors, `root`
/// itself) leave everything, and so does a root that is not `identity`,
/// compared on the descriptor-relative stat the removal starts from.
pub(super) fn remove_each(
    root: &Path,
    one_device: bool,
    identity: Option<FileIdentity>,
) -> TreeRemoval {
    remove_each_observed(root, one_device, identity, &mut |_, _| {})
}

fn remove_observed(root: &Path, one_device: bool, observer: Observer<'_>) -> io::Result<()> {
    let mut sink = Sink {
        best_effort: false,
        removal: TreeRemoval::default(),
    };
    remove_with(root, one_device, None, observer, &mut sink)
}

fn remove_each_observed(
    root: &Path,
    one_device: bool,
    identity: Option<FileIdentity>,
    observer: Observer<'_>,
) -> TreeRemoval {
    let mut sink = Sink {
        best_effort: true,
        removal: TreeRemoval::default(),
    };
    if let Err(error) = remove_with(root, one_device, identity, observer, &mut sink) {
        sink.removal.record(root, error);
    }
    sink.removal
}

fn remove_with(
    root: &Path,
    one_device: bool,
    identity: Option<FileIdentity>,
    observer: Observer<'_>,
    sink: &mut Sink,
) -> io::Result<()> {
    let mut names = Vec::new();
    for component in root.components() {
        match component {
            Component::Normal(name) => names.push(name),
            Component::RootDir | Component::CurDir => {}
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "path contains '..'",
                ))
            }
        }
    }
    let (leaf, ancestors) = names
        .split_last()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "refusing a filesystem root"))?;
    let start = if root.is_absolute() { "/" } else { "." };
    let mut parent = open_directory(libc::AT_FDCWD, &name(OsStr::new(start))?, SEARCH_ACCESS)?;
    let mut shown = PathBuf::from(start);
    for ancestor in ancestors {
        shown.push(ancestor);
        observer(Stage::BeforeAncestorOpen, &shown);
        parent = match open_directory(parent.as_raw_fd(), &name(ancestor)?, SEARCH_ACCESS) {
            Ok(fd) => fd,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(error) => return Err(error),
        };
    }
    let leaf = name(leaf)?;
    let expected = match stat_at(parent.as_raw_fd(), &leaf) {
        Ok(stat) => stat,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if identity.is_some_and(|identity| !identity.matches(&expected)) {
        return Err(changed());
    }
    remove_entry(
        parent.as_raw_fd(),
        &leaf,
        &expected,
        one_device.then_some(expected.st_dev),
        root,
        observer,
        sink,
    )
}

fn name(value: &OsStr) -> io::Result<CString> {
    CString::new(value.as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL"))
}

pub(crate) fn changed() -> io::Error {
    io::Error::other(EntryChanged)
}

fn same_entry(a: &libc::stat, b: &libc::stat) -> bool {
    a.st_dev == b.st_dev
        && a.st_ino == b.st_ino
        && a.st_mode & libc::S_IFMT == b.st_mode & libc::S_IFMT
}

fn stat_at(parent: RawFd, name: &CStr) -> io::Result<libc::stat> {
    let mut stat = std::mem::MaybeUninit::uninit();
    // SAFETY: parent is borrowed and live; name is NUL-terminated, stat is writable.
    if unsafe {
        libc::fstatat(
            parent,
            name.as_ptr(),
            stat.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    } != 0
    {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fstatat initialized stat.
    Ok(unsafe { stat.assume_init() })
}

fn stat_fd(fd: RawFd) -> io::Result<libc::stat> {
    let mut stat = std::mem::MaybeUninit::uninit();
    // SAFETY: fd is borrowed and live; stat is writable.
    if unsafe { libc::fstat(fd, stat.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fstat initialized stat.
    Ok(unsafe { stat.assume_init() })
}

fn open_directory(parent: RawFd, name: &CStr, access: libc::c_int) -> io::Result<OwnedFd> {
    // SAFETY: parent is live or AT_FDCWD; no O_CREAT, so no mode argument is needed.
    let fd = unsafe { libc::openat(parent, name.as_ptr(), DIRECTORY_FLAGS | access) };
    if fd < 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful openat returns a newly owned descriptor.
    Ok(unsafe { OwnedFd::from_raw_fd(fd) })
}

/// Removes one entry. With a best-effort `sink`, a child that fails is
/// recorded and its siblings still go; the folder holding it then stays
/// (`ENOTEMPTY`) without a failure of its own. `Err` is always this entry's
/// own failure.
fn remove_entry(
    parent: RawFd,
    name: &CStr,
    expected: &libc::stat,
    device: Option<libc::dev_t>,
    shown: &Path,
    observer: Observer<'_>,
    sink: &mut Sink,
) -> io::Result<()> {
    let directory = expected.st_mode & libc::S_IFMT == libc::S_IFDIR;
    if !directory
        && !matches!(
            expected.st_mode & libc::S_IFMT,
            libc::S_IFREG | libc::S_IFLNK
        )
    {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "not a regular file, link or directory",
        ));
    }
    let failed_before = sink.removal.failed_entries;
    let mut opened_stream = None;
    if directory {
        observer(Stage::BeforeDirectoryOpen, shown);
        let fd = match open_directory(parent, name, libc::O_RDONLY) {
            Ok(fd) => fd,
            // Gone since its stat: nothing of it is left.
            Err(error) if error.raw_os_error() == Some(libc::ENOENT) => return Ok(()),
            // Now a symlink (`O_NOFOLLOW`) or no longer a folder.
            Err(error) if matches!(error.raw_os_error(), Some(libc::ELOOP | libc::ENOTDIR)) => {
                return Err(changed())
            }
            Err(error) => return Err(error),
        };
        let opened = stat_fd(fd.as_raw_fd())?;
        if !same_entry(expected, &opened) {
            return Err(changed());
        }
        check_device(device, &opened)?;
        observer(Stage::AfterDirectoryOpen, shown);
        let mut stream = Directory::new(fd)?;
        loop {
            let child = match stream.next() {
                Ok(Some(child)) => child,
                Ok(None) => break,
                // The listing cannot go on; what it did not reach stays.
                Err(error) => {
                    sink.fail(shown, error)?;
                    break;
                }
            };
            let child_path = shown.join(OsStr::from_bytes(child.to_bytes()));
            let child_stat = match stat_at(stream.fd(), &child) {
                Ok(stat) => stat,
                Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
                Err(error) => {
                    sink.fail(&child_path, error)?;
                    continue;
                }
            };
            if let Err(error) = remove_entry(
                stream.fd(),
                &child,
                &child_stat,
                device,
                &child_path,
                observer,
                sink,
            ) {
                sink.fail(&child_path, error)?;
            }
        }
        opened_stream = Some(stream);
    }
    observer(Stage::BeforeUnlink, shown);
    match stat_at(parent, name) {
        Ok(now) if same_entry(expected, &now) => {}
        Ok(_) => return Err(changed()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    }
    // SAFETY: parent is live; name is one entry. unlinkat never follows its final symlink.
    let rc = unsafe {
        libc::unlinkat(
            parent,
            name.as_ptr(),
            if directory { libc::AT_REMOVEDIR } else { 0 },
        )
    };
    if rc == 0 {
        sink.removal.removed_entries += 1;
        return match opened_stream {
            Some(stream) => stream.close(),
            None => Ok(()),
        };
    }
    let error = io::Error::last_os_error();
    let left_inside = sink.removal.failed_entries > failed_before;
    match error.raw_os_error() {
        Some(libc::ENOENT) => Ok(()),
        // Kept only because a recorded entry inside it stayed.
        Some(libc::ENOTEMPTY) if left_inside => Ok(()),
        _ => Err(error),
    }
}

fn check_device(device: Option<libc::dev_t>, opened: &libc::stat) -> io::Result<()> {
    if device.is_some_and(|device| device != opened.st_dev) {
        return Err(io::Error::from_raw_os_error(libc::EXDEV));
    }
    Ok(())
}

struct Directory(Option<NonNull<libc::DIR>>);

impl Directory {
    fn new(fd: OwnedFd) -> io::Result<Self> {
        // SAFETY: fd is an owned directory. Only success transfers it to DIR.
        let raw = unsafe { libc::fdopendir(fd.as_raw_fd()) };
        let dir = NonNull::new(raw).ok_or_else(io::Error::last_os_error)?;
        let _ = fd.into_raw_fd();
        Ok(Self(Some(dir)))
    }

    fn fd(&self) -> RawFd {
        // SAFETY: this stream is live; returned fd is only borrowed for *at calls.
        unsafe { libc::dirfd(self.0.unwrap().as_ptr()) }
    }

    fn next(&mut self) -> io::Result<Option<CString>> {
        loop {
            // SAFETY: errno is thread-local, and DIR is exclusively borrowed here.
            let entry = unsafe {
                *errno() = 0;
                libc::readdir(self.0.unwrap().as_ptr())
            };
            if entry.is_null() {
                let error = io::Error::last_os_error();
                return if error.raw_os_error() == Some(0) {
                    Ok(None)
                } else {
                    Err(error)
                };
            }
            // SAFETY: readdir's name is NUL-terminated and valid until the next read/close.
            let name = unsafe { CStr::from_ptr((*entry).d_name.as_ptr()) };
            if matches!(name.to_bytes(), b"." | b"..") {
                continue;
            }
            return Ok(Some(name.to_owned()));
        }
    }

    fn close(mut self) -> io::Result<()> {
        let dir = self.0.take().unwrap();
        // SAFETY: taking the pointer gives this call the only close; Drop will not retry it.
        if unsafe { libc::closedir(dir.as_ptr()) } == 0 {
            Ok(())
        } else {
            Err(io::Error::last_os_error())
        }
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        if let Some(dir) = self.0.take() {
            // SAFETY: this wrapper owns the stream and its fd; no close has occurred.
            unsafe {
                libc::closedir(dir.as_ptr());
            }
        }
    }
}

unsafe fn errno() -> *mut libc::c_int {
    #[cfg(any(target_os = "macos", target_os = "ios", target_os = "freebsd"))]
    {
        unsafe { libc::__error() }
    }
    #[cfg(target_os = "linux")]
    {
        unsafe { libc::__errno_location() }
    }
    #[cfg(target_os = "android")]
    {
        unsafe { libc::__errno() }
    }
}

#[cfg(test)]
mod tests;
