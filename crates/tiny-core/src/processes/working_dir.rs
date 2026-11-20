//! A process's current working directory, read from `proc_pidinfo`.

/// The directory `pid` runs from, for telling which folder an agent session works in.
/// `None` when the read fails: macOS denies it for another user's process, the process
/// is gone or has no working directory, or the platform has no `proc_pidinfo`. Never an
/// empty path.
#[cfg(target_os = "macos")]
pub fn working_directory(pid: u32) -> Option<String> {
    String::from_utf8(working_directory_bytes(pid)?).ok()
}

/// The raw bytes of `pid`'s working directory, as `proc_pidinfo` gives them;
/// `None` as for `working_directory`, never empty.
#[cfg(target_os = "macos")]
pub(crate) fn working_directory_bytes(pid: u32) -> Option<Vec<u8>> {
    let pid = libc::c_int::try_from(pid).ok()?;
    // SAFETY: an all-zero `proc_vnodepathinfo` is a valid value of plain integers.
    let mut info: libc::proc_vnodepathinfo = unsafe { std::mem::zeroed() };
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_vnodepathinfo>()).ok()?;
    // SAFETY: `info` is the structure the PROC_PIDVNODEPATHINFO flavor fills, `size` its size.
    let written = unsafe {
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDVNODEPATHINFO,
            0,
            (&mut info as *mut libc::proc_vnodepathinfo).cast(),
            size,
        )
    };
    if written != size {
        return None;
    }
    // The path is a NUL-terminated C string stored as 32 rows of 32 `c_char`.
    let bytes: Vec<u8> = info
        .pvi_cdir
        .vip_path
        .iter()
        .flatten()
        .map(|&byte| byte as u8)
        .take_while(|&byte| byte != 0)
        .collect();
    (!bytes.is_empty()).then_some(bytes)
}

/// The working directory of every process this user may read, each folder
/// once, in no particular order. A process macOS will not describe (another
/// user's, or one that exits during the walk) is left out, so the list is a
/// floor: a caller that must know a folder is unused also asks about the
/// processes it cares about (`is_running`).
#[cfg(target_os = "macos")]
pub fn all_working_directories() -> Vec<std::path::PathBuf> {
    use std::os::unix::ffi::OsStringExt;
    let mut folders: Vec<std::path::PathBuf> = all_pids()
        .into_iter()
        .filter_map(working_directory_bytes)
        .map(|bytes| std::ffi::OsString::from_vec(bytes).into())
        .collect();
    folders.sort();
    folders.dedup();
    folders
}

/// Every PID on the machine, read with room for the processes that start
/// while it is read.
#[cfg(target_os = "macos")]
pub(crate) fn all_pids() -> Vec<u32> {
    // SAFETY: a null buffer asks only for the count.
    let count = unsafe { libc::proc_listallpids(std::ptr::null_mut(), 0) };
    let Ok(count) = usize::try_from(count) else {
        return Vec::new();
    };
    let mut pids: Vec<libc::c_int> = vec![0; count + count / 4 + 16];
    let bytes = libc::c_int::try_from(pids.len() * std::mem::size_of::<libc::c_int>())
        .unwrap_or(libc::c_int::MAX);
    // SAFETY: `pids` holds `bytes` bytes of `c_int`s, which the call fills.
    let filled = unsafe { libc::proc_listallpids(pids.as_mut_ptr().cast(), bytes) };
    let filled = usize::try_from(filled).unwrap_or(0).min(pids.len());
    pids[..filled]
        .iter()
        .filter_map(|&pid| u32::try_from(pid).ok())
        .filter(|&pid| pid != 0)
        .collect()
}

/// Whether `pid` names a live process, readable or not (`kill` with no
/// signal: `EPERM` is another user's live process).
#[cfg(target_os = "macos")]
pub fn is_running(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };
    // SAFETY: signal 0 only checks that the process exists and may be signalled.
    let rc = unsafe { libc::kill(pid, 0) };
    rc == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

#[cfg(not(target_os = "macos"))]
pub fn working_directory(_pid: u32) -> Option<String> {
    None
}

#[cfg(not(target_os = "macos"))]
pub fn all_working_directories() -> Vec<std::path::PathBuf> {
    Vec::new()
}

#[cfg(not(target_os = "macos"))]
pub fn is_running(_pid: u32) -> bool {
    false
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::{all_working_directories, is_running, working_directory};

    #[test]
    fn every_readable_process_folder_is_listed_once() {
        let folder = tempfile::tempdir().unwrap();
        let folder = folder.path().canonicalize().unwrap();
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("5")
            .current_dir(&folder)
            .spawn()
            .unwrap();
        let all = all_working_directories();
        let running = is_running(child.id());
        child.kill().unwrap();
        child.wait().unwrap();
        assert!(
            all.contains(&folder),
            "{} not in the list",
            folder.display()
        );
        assert_eq!(all.iter().filter(|f| **f == folder).count(), 1);
        assert!(running);
        assert!(!is_running(u32::MAX));
    }

    #[test]
    fn own_working_directory_is_read() {
        let expected = std::env::current_dir().unwrap().canonicalize().unwrap();
        let read = working_directory(std::process::id()).expect("own process is readable");
        assert_eq!(
            std::path::Path::new(&read).canonicalize().unwrap(),
            expected
        );
    }

    #[test]
    fn a_child_reports_the_folder_it_was_started_in() {
        let folder = std::env::temp_dir().canonicalize().unwrap();
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("5")
            .current_dir(&folder)
            .spawn()
            .unwrap();
        let read = working_directory(child.id());
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(read.as_deref(), folder.to_str());
    }

    #[test]
    fn another_users_process_and_a_missing_pid_are_unreadable() {
        // launchd (pid 1) belongs to root, so only a root test run may read it.
        // SAFETY: getuid has no preconditions and cannot fail.
        let root = unsafe { libc::getuid() } == 0;
        assert_eq!(working_directory(1).is_some(), root);
        assert_eq!(working_directory(u32::MAX), None);
    }
}
