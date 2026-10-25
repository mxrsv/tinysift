//! Keep the leader unreaped until pipe draining and group signalling finish.
//!
//! `try_wait` reaps on Unix, releasing the PID that also names our process group.
//! `waitid(WNOWAIT)` observes exit without releasing that identity. No other code
//! may wait for these children (including a process-wide SIGCHLD reaper).

use std::io;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStderr, ChildStdout, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use super::POLL_INTERVAL;

pub(super) struct ChildLifecycle {
    child: Child,
    own_group: bool,
    released: bool,
}

impl ChildLifecycle {
    pub(super) fn spawn(mut command: Command, args: &[&str], own_group: bool) -> io::Result<Self> {
        if own_group {
            command.process_group(0);
        }
        Self::start(command, args, own_group)
    }

    /// Starts `command` as the leader of a new session, which leads a process
    /// group of its own too. `setsid` fails for a process that already leads
    /// a group, so no `process_group(0)` here.
    pub(super) fn spawn_session(mut command: Command, args: &[&str]) -> io::Result<Self> {
        // SAFETY: the closure runs between fork and exec and calls only
        // setsid(2), which is async-signal-safe and touches no memory.
        unsafe {
            command.pre_exec(|| match libc::setsid() {
                -1 => Err(io::Error::last_os_error()),
                _ => Ok(()),
            });
        }
        Self::start(command, args, true)
    }

    fn start(mut command: Command, args: &[&str], own_group: bool) -> io::Result<Self> {
        let child = command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        Ok(Self {
            child,
            own_group,
            released: false,
        })
    }

    pub(super) fn stdout(&mut self) -> Option<ChildStdout> {
        self.child.stdout.take()
    }
    pub(super) fn stderr(&mut self) -> Option<ChildStderr> {
        self.child.stderr.take()
    }

    /// Observe only: the leader, including a zombie, still reserves its PID/PGID.
    pub(super) fn exited(&mut self) -> io::Result<bool> {
        if self.released {
            return Ok(true);
        }
        loop {
            // SAFETY: all-zero siginfo_t is valid storage; waitid fills it for this
            // owned child only. WNOWAIT explicitly leaves it available for wait().
            let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
            let result = unsafe {
                libc::waitid(
                    libc::P_PID,
                    self.child.id() as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOHANG | libc::WNOWAIT,
                )
            };
            if result == 0 {
                // SAFETY: waitid initialized the SIGCHLD fields; zero means no event.
                return Ok(unsafe { info.si_pid() } != 0);
            }
            let error = io::Error::last_os_error();
            if error.kind() == io::ErrorKind::Interrupted {
                continue;
            }
            // An external reaper violates ownership. Fail closed: never signal a
            // PID/PGID that may already have been recycled.
            if error.raw_os_error() == Some(libc::ECHILD) {
                self.released = true;
            }
            return Err(error);
        }
    }

    pub(super) fn wait(&mut self) -> io::Result<ExitStatus> {
        let result = self.child.wait();
        if result.is_ok()
            || result.as_ref().err().and_then(io::Error::raw_os_error) == Some(libc::ECHILD)
        {
            self.released = true;
        }
        result
    }

    fn signal(&mut self, signal: libc::c_int, group: bool) -> io::Result<()> {
        self.exited()?;
        if self.released {
            return Ok(());
        }
        let pid = self.child.id() as libc::pid_t;
        // SAFETY: this Child has not been reaped; its PID cannot be reused. In
        // app mode it is also the PGID we created. CLI mode signals only it.
        let result = unsafe { libc::kill(if group { -pid } else { pid }, signal) };
        if result == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
    }

    pub(super) fn kill(&mut self) -> io::Result<()> {
        let group = self.signal(libc::SIGKILL, self.own_group);
        // A tool may have moved itself out of the original group. Its direct
        // PID is still ours until wait(), so retain the old runner's fallback.
        let (signalled, leader_signalled) = if self.own_group {
            let direct = self.signal(libc::SIGKILL, false);
            let leader_signalled = direct.is_ok();
            (group.and(direct), leader_signalled)
        } else {
            let leader_signalled = group.is_ok();
            (group, leader_signalled)
        };
        // Even a failed group signal must reap a leader that is already dead.
        // Do not block waiting for a live child we could not signal.
        if leader_signalled || self.exited().unwrap_or(false) {
            let waited = self.wait().map(|_| ());
            signalled.and(waited)
        } else {
            signalled
        }
    }

    /// A leader exiting after SIGINT does not mean its descendants finished.
    /// Give inherited pipes the grace period too, then stop the remaining group
    /// before reaping the leader. CLI mode retains direct-child-only semantics.
    pub(super) fn interrupt(
        &mut self,
        grace: Duration,
        drained: impl Fn() -> bool,
    ) -> io::Result<()> {
        if self.exited()? && (!self.own_group || drained()) {
            return self.wait().map(|_| ());
        }
        if let Err(error) = self.signal(libc::SIGINT, self.own_group) {
            // The leader and last pipe can finish between the check and signal.
            // Darwin then reports EPERM for its zombie-only process group.
            if self.exited()? && (!self.own_group || drained()) {
                return self.wait().map(|_| ());
            }
            return Err(error);
        }
        let until = Instant::now() + grace;
        while Instant::now() < until {
            if self.exited()? && (!self.own_group || drained()) {
                // Darwin refuses a group signal once only its zombie leader
                // remains. Finished pipes and leader need no escalation.
                return self.wait().map(|_| ());
            }
            thread::sleep(POLL_INTERVAL);
        }
        self.kill()
    }
}

impl Drop for ChildLifecycle {
    fn drop(&mut self) {
        if !self.released {
            // Error/unwind paths must stop the owned child/group before reaping.
            let _ = self.kill();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observing_exit_does_not_reap_until_wait() {
        let mut child =
            ChildLifecycle::spawn(Command::new("/bin/sh"), &["-c", "exit 7"], true).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        while !child.exited().unwrap() {
            assert!(Instant::now() < deadline, "fixture did not exit");
            thread::sleep(POLL_INTERVAL);
        }
        assert!(
            child.exited().unwrap(),
            "WNOWAIT must leave exit observable"
        );
        assert_eq!(child.wait().unwrap().code(), Some(7));
        // SAFETY: waitpid only checks this already-reaped fixture's child status.
        let result = unsafe {
            libc::waitpid(
                child.child.id() as libc::pid_t,
                std::ptr::null_mut(),
                libc::WNOHANG,
            )
        };
        assert_eq!(result, -1);
        assert_eq!(
            io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }

    #[test]
    fn killing_a_leader_that_moved_groups_still_stops_the_owned_pid() {
        let mut command = Command::new("/bin/sleep");
        // SAFETY: getpgrp has no memory arguments. The pre_exec hook calls only
        // async-signal-safe setpgid with this copied ID, then immediately execs.
        let parent_group = unsafe { libc::getpgrp() };
        unsafe {
            command.pre_exec(move || {
                if libc::setpgid(0, parent_group) == 0 {
                    Ok(())
                } else {
                    Err(io::Error::last_os_error())
                }
            });
        }
        let mut child = ChildLifecycle::spawn(command, &["1"], true).unwrap();
        let started = Instant::now();
        child.kill().unwrap();
        assert!(
            started.elapsed() < Duration::from_millis(500),
            "moved leader was not killed"
        );
    }
}
