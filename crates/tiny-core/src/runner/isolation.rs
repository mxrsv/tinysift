//! A tool that runs with nothing of the app's own around it.
//!
//! `ToolLookup::isolated` is for a tool that can ask for a password or read
//! its settings from the environment, such as `brew`: its environment is
//! cleared down to `HOME`, a fixed `PATH` and what the lookup adds, and it
//! starts in a session of its own. With no controlling terminal a `sudo` it
//! runs fails at once instead of waiting for someone to type. The session
//! leader is also the leader of a process group the runner signals, so a
//! timeout or a cancel reaches what the tool started.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Isolation {
    /// The account's home. `None` leaves the tool unable to run: no `HOME`
    /// would make it guess, and the environment is not asked.
    home: Option<PathBuf>,
    path: OsString,
}

impl Isolation {
    pub(super) fn new(home: Option<PathBuf>, path: &str) -> Self {
        Self {
            home,
            path: path.into(),
        }
    }

    /// Clears `command`'s environment down to `HOME` and `PATH`. `None` when
    /// there is no home to give.
    pub(super) fn apply(&self, command: &mut Command) -> Option<()> {
        let home = self.home.as_ref()?;
        command
            .env_clear()
            .env("HOME", home)
            .env("PATH", &self.path);
        Some(())
    }
}

#[cfg(test)]
mod tests;
