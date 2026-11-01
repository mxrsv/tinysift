//! A process's current working directory, read from `proc_pidinfo`.

/// The directory `pid` runs from, for telling which folder an agent session works in.
/// `None` when the read fails: macOS denies it for another user's process, the process
/// is gone or has no working directory, or the platform has no `proc_pidinfo`. Never an
/// empty path.
#[cfg(target_os = "macos")]
pub fn working_directory(pid: u32) -> Option<String> {
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
    String::from_utf8(bytes)
        .ok()
        .filter(|path| !path.is_empty())
}

#[cfg(not(target_os = "macos"))]
pub fn working_directory(_pid: u32) -> Option<String> {
    None
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use super::working_directory;

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
