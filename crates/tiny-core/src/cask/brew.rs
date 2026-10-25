//! Finding the user's `brew` and running it with a fixed environment.

use std::path::{Path, PathBuf};
use std::time::Duration;

use super::failure::CaskFailure;
use crate::runner::{CommandOutcome, CommandRunner, ToolLookup};

const BREW: &str = "brew";

/// Set for every `brew` the app runs, read or write. Each entry was checked
/// against the Homebrew 7.0.10 source (`Library/Homebrew`).
///
/// - `HOMEBREW_NO_AUTO_UPDATE`: no `brew update` before the command. With it
///   set `api.rb` never refreshes the cask index, so the version and host a
///   preview shows are the ones `brew install` then uses. The price is an
///   index as old as the user's last `brew update`.
/// - `HOMEBREW_NO_SUDO`: `cask/installer.rb` refuses a cask whose artifact
///   needs `sudo` before it downloads, and `system_command.rb` fails any
///   later `sudo`, so `brew` cannot block on a password nobody can type.
/// - `HOMEBREW_NO_INSTALL_CLEANUP`: an install or upgrade otherwise may clean
///   up unrelated packages, beyond the one command a preview names.
/// - `HOMEBREW_NO_ASK`, `_ANALYTICS`, `_ENV_HINTS`, `_COLOR`, `_EMOJI`: no
///   plan prompt, no analytics event, and plain `==> ` lines to read.
/// - `NONINTERACTIVE`: the convention third-party installer scripts honour.
///   `brew` itself never reads it; its prompts are skipped because stdin is
///   closed (`ask.rb` returns without a TTY).
pub const BREW_ENV: &[(&str, &str)] = &[
    ("HOMEBREW_NO_AUTO_UPDATE", "1"),
    ("HOMEBREW_NO_SUDO", "1"),
    ("HOMEBREW_NO_INSTALL_CLEANUP", "1"),
    ("HOMEBREW_NO_ASK", "1"),
    ("HOMEBREW_NO_ANALYTICS", "1"),
    ("HOMEBREW_NO_ENV_HINTS", "1"),
    ("HOMEBREW_NO_COLOR", "1"),
    ("HOMEBREW_NO_EMOJI", "1"),
    ("NONINTERACTIVE", "1"),
];

/// Whether the Mac has a Homebrew to run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HomebrewStatus {
    /// Absolute path of the `brew` executable.
    Present(PathBuf),
    Missing,
}

/// Where the app looks for `brew`: `PATH`, then the Apple silicon prefix
/// (`/opt/homebrew/bin`) and the Intel one (`/usr/local/bin`), because a
/// Finder-launched app has a minimal `PATH`. Runs it with [`BREW_ENV`].
pub fn brew_lookup() -> ToolLookup {
    ToolLookup::app().with_env(BREW_ENV)
}

/// Finds an existing `brew`; never installs one.
pub fn locate_brew(lookup: &ToolLookup) -> HomebrewStatus {
    match lookup.resolve(BREW) {
        Some(path) => HomebrewStatus::Present(path),
        None => HomebrewStatus::Missing,
    }
}

/// A `brew` that was found, run through a [`CommandRunner`] so tests can
/// replace the process.
pub struct Brew<'a> {
    path: &'a Path,
    runner: &'a dyn CommandRunner,
}

impl<'a> Brew<'a> {
    pub fn new(path: &'a Path, runner: &'a dyn CommandRunner) -> Self {
        Self { path, runner }
    }

    pub fn path(&self) -> &Path {
        self.path
    }

    /// Runs `brew args...` to completion within `timeout`. A non-zero exit is
    /// an outcome here; the caller classifies it.
    pub(super) fn output(
        &self,
        args: &[&str],
        timeout: Duration,
    ) -> Result<CommandOutcome, CaskFailure> {
        let bin = self.path.to_str().ok_or(CaskFailure::HomebrewMissing)?;
        self.runner
            .output(bin, args, timeout)
            .map_err(CaskFailure::from)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::os::unix::fs::PermissionsExt;

    use crate::runner::APP_FALLBACK_DIRS;

    fn brew_in(dir: &Path, mode: u32) {
        let path = dir.join("brew");
        std::fs::write(&path, "#!/bin/sh\n").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(mode)).unwrap();
    }

    fn lookup(path: &Path, prefixes: &[&Path]) -> ToolLookup {
        ToolLookup::new(
            Some(OsString::from(path)),
            prefixes.iter().map(|p| p.to_path_buf()).collect(),
        )
    }

    #[test]
    fn finds_brew_in_either_prefix_and_reports_missing_otherwise() {
        let silicon = tempfile::tempdir().unwrap();
        let intel = tempfile::tempdir().unwrap();
        let empty = tempfile::tempdir().unwrap();
        brew_in(silicon.path(), 0o755);
        brew_in(intel.path(), 0o755);
        let both = lookup(empty.path(), &[silicon.path(), intel.path()]);
        assert_eq!(
            locate_brew(&both),
            HomebrewStatus::Present(silicon.path().join("brew")),
            "Apple silicon first"
        );
        let intel_only = lookup(empty.path(), &[empty.path(), intel.path()]);
        assert_eq!(
            locate_brew(&intel_only),
            HomebrewStatus::Present(intel.path().join("brew"))
        );
        assert_eq!(
            locate_brew(&lookup(empty.path(), &[empty.path()])),
            HomebrewStatus::Missing
        );
    }

    #[test]
    fn a_brew_that_cannot_run_is_missing() {
        let prefix = tempfile::tempdir().unwrap();
        brew_in(prefix.path(), 0o644);
        let lookup = lookup(prefix.path(), &[prefix.path()]);
        assert_eq!(locate_brew(&lookup), HomebrewStatus::Missing);
    }

    #[test]
    fn the_app_searches_both_homebrew_prefixes() {
        assert_eq!(APP_FALLBACK_DIRS, ["/opt/homebrew/bin", "/usr/local/bin"]);
    }

    #[test]
    fn brew_never_updates_prompts_or_asks_for_a_password() {
        let set = |key: &str| BREW_ENV.iter().find(|(k, _)| *k == key).map(|(_, v)| *v);
        for key in [
            "HOMEBREW_NO_AUTO_UPDATE",
            "HOMEBREW_NO_SUDO",
            "HOMEBREW_NO_INSTALL_CLEANUP",
            "HOMEBREW_NO_ASK",
            "NONINTERACTIVE",
        ] {
            assert_eq!(set(key), Some("1"), "{key}");
        }
    }
}
