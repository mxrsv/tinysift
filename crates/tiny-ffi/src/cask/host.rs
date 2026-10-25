//! Where `brew` comes from. Production finds the user's own and runs it
//! through the bounded runner; tests inject a fake that spawns nothing.

use std::path::PathBuf;

use tiny_core::cask::{brew_lookup, locate_brew, HomebrewStatus};
use tiny_core::clean::trash_plan::account_home;
use tiny_core::runner::{CommandRunner, ToolLookup, ToolRunner};

pub(crate) trait BrewHost {
    fn status(&self) -> HomebrewStatus;
    /// What every `brew` call goes through.
    fn runner(&self) -> &dyn CommandRunner;
    /// The account's home, whose `~/.homebrew/brew.env` `brew` reads.
    fn home(&self) -> Option<PathBuf>;
}

/// The user's `brew`, run alone (`tiny_core::cask::brew_lookup`).
pub(crate) struct SystemBrew {
    lookup: ToolLookup,
    runner: ToolRunner,
}

impl SystemBrew {
    pub(crate) fn new() -> Self {
        let lookup = brew_lookup();
        Self {
            runner: ToolRunner::new(lookup.clone()),
            lookup,
        }
    }
}

impl BrewHost for SystemBrew {
    fn status(&self) -> HomebrewStatus {
        locate_brew(&self.lookup)
    }

    fn runner(&self) -> &dyn CommandRunner {
        &self.runner
    }

    fn home(&self) -> Option<PathBuf> {
        account_home()
    }
}
