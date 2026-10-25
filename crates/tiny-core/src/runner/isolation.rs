//! A tool that runs with nothing of the app's own around it.
//!
//! `ToolLookup::isolated` is for a tool that can ask for a password or read
//! its settings from the environment, such as `brew`: its environment is
//! cleared down to `HOME`, a fixed `PATH`, the proxy variables the process
//! has (`PASS_THROUGH`) and what the lookup adds, and it
//! starts in a session of its own. With no controlling terminal a `sudo` it
//! runs fails at once instead of waiting for someone to type. The session
//! leader is also the leader of a process group the runner signals, so a
//! timeout or a cancel reaches what the tool started.

use std::ffi::OsString;
use std::path::PathBuf;
use std::process::Command;

/// The only variables handed on from the app's own environment, unchanged
/// when set and not empty: a proxy the user's network needs, which `curl`
/// reads. Without them a user behind a proxy sees every download fail. Nothing
/// else is passed on; `brew.env` files can still set more, and `brew` applies
/// those itself.
const PASS_THROUGH: &[&str] = &[
    "http_proxy",
    "https_proxy",
    "all_proxy",
    "no_proxy",
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "NO_PROXY",
];

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

    /// Clears `command`'s environment down to `HOME`, `PATH` and the proxy
    /// variables the process has. `None` when there is no home to give.
    pub(super) fn apply(&self, command: &mut Command) -> Option<()> {
        self.apply_from(command, |name| std::env::var_os(name))
    }

    fn apply_from(
        &self,
        command: &mut Command,
        process_env: impl Fn(&str) -> Option<OsString>,
    ) -> Option<()> {
        let home = self.home.as_ref()?;
        command
            .env_clear()
            .env("HOME", home)
            .env("PATH", &self.path);
        for name in PASS_THROUGH {
            if let Some(value) = process_env(name).filter(|value| !value.is_empty()) {
                command.env(name, value);
            }
        }
        Some(())
    }
}

#[cfg(test)]
mod tests;
