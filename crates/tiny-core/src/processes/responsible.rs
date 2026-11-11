//! The process macOS holds responsible for another: the app an XPC service, a helper or a
//! decoder works for. Read through `responsibility_get_pid_responsible_for_pid`, a private
//! libsystem function, so every caller must treat "no answer" as the normal case.

/// The responsible process's PID for `pid`, or `None` when macOS has no answer: the function
/// is missing on this system, the process is gone, or it answered `-1` or `0`. A process can
/// be responsible for itself; this function reports that as it is and leaves the judgement to
/// the caller.
#[cfg(target_os = "macos")]
pub(crate) fn read(pid: u32) -> Option<u32> {
    let pid = libc::pid_t::try_from(pid).ok()?;
    let lookup = function()?;
    // SAFETY: `lookup` is the libsystem function of this signature; it only reads `pid`.
    let answer = unsafe { lookup(pid) };
    u32::try_from(answer).ok().filter(|pid| *pid > 0)
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn read(_pid: u32) -> Option<u32> {
    None
}

#[cfg(target_os = "macos")]
type Lookup = unsafe extern "C" fn(libc::pid_t) -> libc::pid_t;

/// The private function, looked up once. `None` when this macOS does not export it.
#[cfg(target_os = "macos")]
fn function() -> Option<Lookup> {
    static FUNCTION: std::sync::OnceLock<Option<Lookup>> = std::sync::OnceLock::new();
    *FUNCTION.get_or_init(|| {
        // SAFETY: the name is a NUL-terminated C string; RTLD_DEFAULT searches every loaded image.
        let symbol = unsafe {
            libc::dlsym(
                libc::RTLD_DEFAULT,
                c"responsibility_get_pid_responsible_for_pid".as_ptr(),
            )
        };
        // SAFETY: a non-null result is the address of the function, whose C signature is
        // `pid_t responsibility_get_pid_responsible_for_pid(pid_t)`.
        (!symbol.is_null())
            .then(|| unsafe { std::mem::transmute::<*mut libc::c_void, Lookup>(symbol) })
    })
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::process::{Command, Stdio};

    use super::read;

    fn is_live(pid: u32) -> bool {
        // SAFETY: signal 0 only checks that the process exists.
        let status = unsafe { libc::kill(pid as libc::pid_t, 0) };
        status == 0 || std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }

    #[test]
    fn own_process_answers_nothing_or_a_live_process() {
        if let Some(pid) = read(std::process::id()) {
            assert!(is_live(pid), "responsible process {pid} is not running");
        }
    }

    #[test]
    fn a_process_that_is_gone_answers_nothing() {
        let mut child = Command::new("/usr/bin/true").spawn().unwrap();
        let pid = child.id();
        child.wait().unwrap();
        assert_eq!(read(pid), None);
        assert_eq!(read(0), None);
        assert_eq!(read(u32::MAX), None);
    }

    #[test]
    fn a_child_shares_its_parents_responsible_process() {
        let mut child = Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let mine = read(std::process::id());
        let its = read(child.id());
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(its, mine);
    }
}
