//! Which paths running processes hold: each readable process's executable,
//! working folder and open files, read in one libproc pass.
//!
//! The answer is a floor. macOS describes only this user's processes, a file
//! a process has mapped without keeping a descriptor is not listed, a process
//! that reads a file and closes it (a runtime loading its modules) holds
//! nothing, and a process may open a file right after the pass. A read that
//! fails is the same as nothing held. "Nothing holds it" therefore never
//! proves a path unused, and a caller that must know has to ask for more than
//! this; "something holds it" is enough to leave it.

use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long one pass answers for. A run asks once per item, and a pass costs
/// about 10 ms on a Mac with 700 processes, so a long run reads again while a
/// burst of items shares one pass.
const SNAPSHOT_MAX_AGE: Duration = Duration::from_secs(2);

/// Processes that read files only to index or back them up. They open
/// whatever is on disk, so their descriptors say nothing about use.
const INDEXERS: &[&str] = &[
    "mds",
    "mds_stores",
    "mdworker",
    "mdworker_shared",
    "mdsync",
    "backupd",
    "quicklookd",
    "QuickLookSatellite",
];

/// Asked right before a removal: is a process still using this path?
pub trait OpenFiles {
    /// The name of a process whose executable, working folder or open file
    /// is `path` or lies inside it; `None` when no readable process has one.
    fn holder_inside(&self, path: &Path) -> Option<String>;

    /// Whether a process runs a file that is `path` or lies inside it. Unlike
    /// `holder_inside` this says when it cannot tell, so a caller that must
    /// keep what it cannot judge can.
    fn running_from(&self, path: &Path) -> Running;
}

/// Whether a running process executes a file at or inside a path.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Running {
    /// The named process does. Certain even when the list is incomplete.
    By(String),
    /// Every live process named its executable, and none is inside.
    Nobody,
    /// The process list, or a live process's executable, could not be read;
    /// the text says which.
    CannotTell(String),
}

/// Answers that nothing is held, for platforms without libproc and for
/// tests that are not about this check.
pub struct NoOpenFiles;

impl OpenFiles for NoOpenFiles {
    fn holder_inside(&self, _path: &Path) -> Option<String> {
        None
    }

    fn running_from(&self, _path: &Path) -> Running {
        Running::Nobody
    }
}

/// One pass's result: each held path with the process holding it.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct HeldPaths {
    held: Vec<(PathBuf, String)>,
    /// The file each process runs, also part of `held`.
    executables: Vec<(PathBuf, String)>,
    /// Why `executables` may be short; `None` when every live process named
    /// its own.
    executables_unread: Option<String>,
}

impl HeldPaths {
    pub fn new(held: impl IntoIterator<Item = (PathBuf, String)>) -> Self {
        Self {
            held: held.into_iter().collect(),
            ..Self::default()
        }
    }

    /// With the files processes run, which `running_from` answers from.
    pub fn with_executables(
        self,
        executables: impl IntoIterator<Item = (PathBuf, String)>,
    ) -> Self {
        let executables: Vec<(PathBuf, String)> = executables.into_iter().collect();
        Self {
            held: self.held.into_iter().chain(executables.clone()).collect(),
            executables,
            ..self
        }
    }

    /// With a reason the list of executables is incomplete.
    pub fn with_executables_unread(self, why: impl Into<String>) -> Self {
        Self {
            executables_unread: Some(why.into()),
            ..self
        }
    }

    /// A process found running from `path` is certain; none found is only
    /// "nobody" when the list is complete.
    pub fn running_from(&self, path: &Path) -> Running {
        let runner = self
            .executables
            .iter()
            .find(|(executable, _)| executable.starts_with(path));
        match (runner, &self.executables_unread) {
            (Some((_, name)), _) => Running::By(name.clone()),
            (None, Some(why)) => Running::CannotTell(why.clone()),
            (None, None) => Running::Nobody,
        }
    }

    /// Every held path that is `path` or lies inside it, with its holder.
    /// Compared component by component, so `/a/cache` never holds
    /// `/a/cache-old`.
    pub fn holders_inside<'a>(
        &'a self,
        path: &'a Path,
    ) -> impl Iterator<Item = (&'a Path, &'a str)> + 'a {
        self.held
            .iter()
            .filter(move |(held, _)| held.starts_with(path))
            .map(|(held, holder)| (held.as_path(), holder.as_str()))
    }

    pub fn holder_inside<'a>(&'a self, path: &'a Path) -> Option<&'a str> {
        self.holders_inside(path).next().map(|(_, holder)| holder)
    }

    /// The holder of a path inside `path` that is still on disk. macOS keeps
    /// naming a file's old path after it is unlinked, so a process holding
    /// only removed files holds nothing a removal would take from it, and an
    /// item that is gone must read as missing, not as in use.
    pub fn live_holder_inside<'a>(&'a self, path: &'a Path) -> Option<&'a str> {
        self.holders_inside(path)
            .find(|(held, _)| std::fs::symlink_metadata(held).is_ok())
            .map(|(_, holder)| holder)
    }
}

impl OpenFiles for HeldPaths {
    fn holder_inside(&self, path: &Path) -> Option<String> {
        HeldPaths::holder_inside(self, path).map(str::to_string)
    }

    fn running_from(&self, path: &Path) -> Running {
        HeldPaths::running_from(self, path)
    }
}

/// The real check: reads the process list when asked, and again once the
/// last pass is older than `SNAPSHOT_MAX_AGE`.
#[derive(Default)]
pub struct ProcessOpenFiles {
    snapshot: Mutex<Option<(Instant, HeldPaths)>>,
}

impl ProcessOpenFiles {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ProcessOpenFiles {
    /// Asks the current pass, reading a new one first when the last is too
    /// old.
    fn ask<R>(&self, question: impl FnOnce(&HeldPaths) -> R) -> R {
        // A poisoned lock only means another thread panicked mid-read; the
        // snapshot inside is replaced whole, never half-written.
        let mut snapshot = self
            .snapshot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let (_, held) = match snapshot.take() {
            Some(pass) if pass.0.elapsed() < SNAPSHOT_MAX_AGE => snapshot.insert(pass),
            _ => snapshot.insert((Instant::now(), read_held_paths())),
        };
        question(held)
    }
}

impl OpenFiles for ProcessOpenFiles {
    fn holder_inside(&self, path: &Path) -> Option<String> {
        self.ask(|held| held.live_holder_inside(path).map(str::to_string))
    }

    fn running_from(&self, path: &Path) -> Running {
        self.ask(|held| held.running_from(path))
    }
}

fn is_indexer(name: &str) -> bool {
    INDEXERS.contains(&name)
}

/// What every process this user may read holds, except this process's own
/// and, for working folders and open files, the indexers'. Which file each
/// process runs is kept apart, with whether that list is complete.
#[cfg(target_os = "macos")]
pub fn read_held_paths() -> HeldPaths {
    use libproc::Executable;

    let own = std::process::id();
    let pids = crate::processes::working_dir::all_pids();
    let list_unread = pids
        .is_empty()
        .then(|| "the process list could not be read".to_string());
    let processes: Vec<(u32, Executable)> = pids
        .into_iter()
        .filter(|&pid| pid != own)
        .map(|pid| (pid, libproc::executable(pid)))
        .collect();
    let unnamed = processes
        .iter()
        .filter(|(_, executable)| matches!(executable, Executable::Unreadable))
        .count();
    let executables: Vec<(PathBuf, String)> = processes
        .iter()
        .filter_map(|(pid, executable)| {
            let path = executable.path()?;
            Some((path.to_path_buf(), process_name(*pid, executable)))
        })
        .collect();
    let held = processes.iter().flat_map(|(pid, executable)| {
        let name = process_name(*pid, executable);
        if is_indexer(&name) {
            return Vec::new();
        }
        libproc::working_directory(*pid)
            .into_iter()
            .chain(libproc::open_files(*pid))
            .map(|path| (path, name.clone()))
            .collect()
    });
    HeldPaths {
        executables_unread: list_unread.or_else(|| {
            (unnamed > 0).then(|| format!("{unnamed} running processes did not name their file"))
        }),
        ..HeldPaths::new(held).with_executables(executables)
    }
}

/// The file name of what `pid` runs, or the pid when macOS will not say.
#[cfg(target_os = "macos")]
fn process_name(pid: u32, executable: &libproc::Executable) -> String {
    executable
        .path()
        .and_then(Path::file_name)
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| format!("process {pid}"))
}

#[cfg(not(target_os = "macos"))]
pub fn read_held_paths() -> HeldPaths {
    HeldPaths::default().with_executables_unread("this platform cannot list running processes")
}

#[cfg(target_os = "macos")]
mod libproc {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::path::PathBuf;

    /// `PROC_PIDFDVNODEPATHINFO` in `<sys/proc_info.h>`; the `libc` crate
    /// does not name it.
    const PROC_PIDFDVNODEPATHINFO: libc::c_int = 2;
    /// Room for descriptors opened between asking for the count and reading.
    const EXTRA_DESCRIPTORS: usize = 32;

    /// `struct proc_fileinfo` in `<sys/proc_info.h>`.
    #[repr(C)]
    struct ProcFileInfo {
        fi_openflags: u32,
        fi_status: u32,
        fi_offset: i64,
        fi_type: i32,
        fi_guardflags: u32,
    }

    /// `struct vnode_fdinfowithpath` in `<sys/proc_info.h>`.
    #[repr(C)]
    struct VnodeFdInfoWithPath {
        pfi: ProcFileInfo,
        pvip: libc::vnode_info_path,
    }

    // The kernel fills exactly this many bytes; a wrong layout would read a
    // path from the wrong offset.
    const _: () = assert!(std::mem::size_of::<VnodeFdInfoWithPath>() == 1200);

    fn path_from(bytes: Vec<u8>) -> Option<PathBuf> {
        (!bytes.is_empty()).then(|| OsString::from_vec(bytes).into())
    }

    /// What macOS says a process runs.
    pub(super) enum Executable {
        At(PathBuf),
        /// The process ended, or is a zombie that runs nothing (`ESRCH`).
        Gone,
        /// A live process whose file macOS would not name.
        Unreadable,
    }

    impl Executable {
        pub(super) fn path(&self) -> Option<&std::path::Path> {
            match self {
                Executable::At(path) => Some(path),
                Executable::Gone | Executable::Unreadable => None,
            }
        }
    }

    /// The file `pid` runs.
    pub(super) fn executable(pid: u32) -> Executable {
        let Ok(pid) = libc::c_int::try_from(pid) else {
            return Executable::Gone;
        };
        let mut buffer = vec![0u8; libc::PROC_PIDPATHINFO_MAXSIZE as usize];
        // SAFETY: `buffer` holds the `buffersize` bytes the call may write.
        let written =
            unsafe { libc::proc_pidpath(pid, buffer.as_mut_ptr().cast(), buffer.len() as u32) };
        let Some(written) = usize::try_from(written).ok().filter(|&n| n > 0) else {
            let gone = std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH);
            return if gone {
                Executable::Gone
            } else {
                Executable::Unreadable
            };
        };
        buffer.truncate(written.min(buffer.len()));
        path_from(buffer).map_or(Executable::Unreadable, Executable::At)
    }

    pub(super) fn working_directory(pid: u32) -> Option<PathBuf> {
        crate::processes::working_dir::working_directory_bytes(pid).and_then(path_from)
    }

    /// The files `pid` has open as descriptors. Empty when macOS will not
    /// list them (another user's process) or the process is gone.
    pub(super) fn open_files(pid: u32) -> Vec<PathBuf> {
        let Ok(pid) = libc::c_int::try_from(pid) else {
            return Vec::new();
        };
        let entry = std::mem::size_of::<libc::proc_fdinfo>();
        // SAFETY: a null buffer asks only for the size of the list.
        let needed =
            unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
        let Ok(needed) = usize::try_from(needed) else {
            return Vec::new();
        };
        if needed == 0 {
            return Vec::new();
        }
        let mut descriptors: Vec<libc::proc_fdinfo> = (0..needed / entry + EXTRA_DESCRIPTORS)
            .map(|_| libc::proc_fdinfo {
                proc_fd: 0,
                proc_fdtype: 0,
            })
            .collect();
        let Ok(bytes) = libc::c_int::try_from(descriptors.len() * entry) else {
            return Vec::new();
        };
        // SAFETY: `descriptors` holds `bytes` bytes of `proc_fdinfo`, which
        // the call fills.
        let filled = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDLISTFDS,
                0,
                descriptors.as_mut_ptr().cast(),
                bytes,
            )
        };
        let filled = usize::try_from(filled).unwrap_or(0) / entry;
        descriptors
            .iter()
            .take(filled)
            .filter(|descriptor| descriptor.proc_fdtype == libc::PROX_FDTYPE_VNODE as u32)
            .filter_map(|descriptor| vnode_path(pid, descriptor.proc_fd))
            .collect()
    }

    fn vnode_path(pid: libc::c_int, descriptor: i32) -> Option<PathBuf> {
        // SAFETY: an all-zero `VnodeFdInfoWithPath` is a valid value of plain
        // integers.
        let mut info: VnodeFdInfoWithPath = unsafe { std::mem::zeroed() };
        let size = libc::c_int::try_from(std::mem::size_of::<VnodeFdInfoWithPath>()).ok()?;
        // SAFETY: `info` is the structure the PROC_PIDFDVNODEPATHINFO flavor
        // fills, `size` its size.
        let written = unsafe {
            libc::proc_pidfdinfo(
                pid,
                descriptor,
                PROC_PIDFDVNODEPATHINFO,
                (&mut info as *mut VnodeFdInfoWithPath).cast(),
                size,
            )
        };
        if written != size {
            return None;
        }
        // The path is a NUL-terminated C string stored as 32 rows of 32
        // `c_char`.
        let bytes: Vec<u8> = info
            .pvip
            .vip_path
            .iter()
            .flatten()
            .map(|&byte| byte as u8)
            .take_while(|&byte| byte != 0)
            .collect();
        path_from(bytes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn held(paths: &[(&str, &str)]) -> HeldPaths {
        HeldPaths::new(
            paths
                .iter()
                .map(|(path, holder)| (PathBuf::from(path), holder.to_string())),
        )
    }

    #[test]
    fn a_path_is_held_by_a_file_inside_it_or_by_itself() {
        let held = held(&[
            ("/h/proj/node_modules/.cache/build.lock", "vite"),
            ("/h/tools/versions/2.1.0", "claude"),
        ]);
        assert_eq!(
            held.holder_inside(Path::new("/h/proj/node_modules")),
            Some("vite")
        );
        assert_eq!(
            held.holder_inside(Path::new("/h/tools/versions/2.1.0")),
            Some("claude")
        );
    }

    #[test]
    fn a_sibling_with_the_same_prefix_or_a_parent_is_not_a_holder() {
        let held = held(&[
            ("/h/cache-old/file", "app"),
            ("/h/proj", "zsh"),
            ("/h/proj/src/main.rs", "editor"),
        ]);
        assert_eq!(held.holder_inside(Path::new("/h/cache")), None);
        assert_eq!(held.holder_inside(Path::new("/h/proj/target")), None);
    }

    #[test]
    fn a_held_path_that_is_gone_holds_nothing() {
        let folder = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(folder.path()).unwrap();
        std::fs::write(root.join("kept.db"), b"data").unwrap();
        let unlinked = HeldPaths::new([(root.join("gone.log"), "tail".to_string())]);
        let both = HeldPaths::new([
            (root.join("gone.log"), "tail".to_string()),
            (root.join("kept.db"), "app".to_string()),
        ]);

        assert_eq!(unlinked.holder_inside(&root), Some("tail"));
        assert_eq!(unlinked.live_holder_inside(&root), None);
        assert_eq!(both.live_holder_inside(&root), Some("app"));
    }

    #[test]
    fn running_from_tells_a_runner_from_nobody_from_cannot_tell() {
        let release = Path::new("/h/tools/versions/2.1.0");
        let old = Path::new("/h/tools/versions/2.0.9");
        let complete =
            HeldPaths::default().with_executables([(release.to_path_buf(), "claude".to_string())]);
        let incomplete = complete
            .clone()
            .with_executables_unread("2 running processes did not name their file");

        assert_eq!(complete.running_from(release), Running::By("claude".into()));
        assert_eq!(complete.running_from(old), Running::Nobody);
        // A runner that was found is certain; only "nobody" needs the whole list.
        assert_eq!(
            incomplete.running_from(release),
            Running::By("claude".into())
        );
        assert!(matches!(
            incomplete.running_from(old),
            Running::CannotTell(_)
        ));
        // The file a process runs is held like any other path it has open.
        assert_eq!(
            complete.holder_inside(Path::new("/h/tools")),
            Some("claude")
        );
        assert_eq!(NoOpenFiles.running_from(release), Running::Nobody);
    }

    #[test]
    fn nothing_held_answers_none() {
        assert_eq!(HeldPaths::default().holder_inside(Path::new("/h")), None);
        assert_eq!(NoOpenFiles.holder_inside(Path::new("/h")), None);
    }

    #[test]
    fn indexers_are_named_exactly() {
        assert!(is_indexer("mdworker_shared"));
        assert!(!is_indexer("mdworker_shared_helper"));
        assert!(!is_indexer("node"));
    }

    /// The real pass, on the one process a test controls: another process
    /// that keeps a temporary file open.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_file_another_process_keeps_open_is_found_and_released_when_it_exits() {
        use std::io::Read;
        use std::process::{Command, Stdio};

        let folder = tempfile::tempdir().unwrap();
        // The real path: the temporary folder sits behind the /var symlink.
        let root = std::fs::canonicalize(folder.path()).unwrap();
        let item = root.join("cache");
        std::fs::create_dir(&item).unwrap();
        let file = item.join("held.db");
        std::fs::write(&file, b"data").unwrap();

        // `tail -f` keeps its file open until it is killed, and prints the
        // contents first, which says the file is open by now.
        let mut holder = Command::new("/usr/bin/tail")
            .arg("-f")
            .arg(&file)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        let mut first = [0u8; 4];
        holder
            .stdout
            .as_mut()
            .unwrap()
            .read_exact(&mut first)
            .unwrap();

        let while_open = read_held_paths();
        assert_eq!(
            while_open.running_from(Path::new("/usr/bin/tail")),
            Running::By("tail".into())
        );
        holder.kill().unwrap();
        holder.wait().unwrap();
        let after_exit = read_held_paths();

        let holders = |held: &HeldPaths| -> Vec<String> {
            held.holders_inside(&item)
                .map(|(_, holder)| holder.to_string())
                .collect()
        };
        // Another process of this user may look at the file too; `tail` is
        // the one this test answers for.
        assert!(holders(&while_open).iter().any(|holder| holder == "tail"));
        assert_eq!(while_open.holder_inside(&root.join("other")), None);
        assert!(!holders(&after_exit).iter().any(|holder| holder == "tail"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn this_process_is_never_its_own_holder() {
        let folder = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(folder.path()).unwrap();
        let file = root.join("mine.db");
        std::fs::write(&file, b"data").unwrap();
        let _open = std::fs::File::open(&file).unwrap();

        assert_eq!(read_held_paths().holder_inside(&root), None);
    }
}
