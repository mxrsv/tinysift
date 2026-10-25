//! Installing and updating apps through Homebrew for the native Apps screen.
//!
//! Swift names a catalogue token or the tokens of an update list, then a
//! preview ID plus the tokens to run, never a path or a command. The session
//! keeps the last update list (`cask_outdated`) and one preview, a plan frozen
//! by core. Execution runs core's `run_install` or `run_updates`; nothing here
//! builds a `brew` command.
//!
//! - **Gate.** Everything that spawns `brew` or reads the apps takes the
//!   session gate and returns `Busy` when another operation holds it. Looking
//!   for `brew` and reading its `brew.env` files does not.
//! - **Single use.** A preview is spent when a run is accepted and expires
//!   after [`PREVIEW_TTL`]. A replay says "already used", not "unknown".
//!   Unlike Uninstall, a run that is refused before it starts for a reason the
//!   user can fix (`AppRunning`, `AppCheckFailed`, `NeedsAdministrator`,
//!   `ManualInstaller`, `AppExists`) leaves the preview unspent: nothing ran,
//!   so quitting the app and pressing the button again is safe. A bad
//!   selection is `InvalidInput` and leaves it usable too.
//! - **Cancel.** A raised token stops a cask that is still downloading. Once
//!   `brew` reports it is installing, the cancel waits for that cask to finish
//!   (an interrupt there can leave an app half replaced) and nothing after it
//!   starts: those casks are `NotAttempted`. The 30-minute run limit is the
//!   only hard stop after that point.
//! - **Open apps.** An open app is never quit for the user. `cask_execute`
//!   returns `Failed { AppRunning }` naming it before `brew` starts; core
//!   checks again right before each upgrade.
//!
//! Locking: session gate, then a short cask-state lock that never spans a
//! `brew` call, a probe or a callback.

use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime};

use tiny_core::cask::{brew_env_warnings, CaskAction, CaskPlanItem, HomebrewStatus, OutdatedCask};
use tiny_core::clean::process::{AppProbe, RunnerProbe};

use crate::clean::app_runner;
use crate::session::CancellationToken;
use crate::uninstall::catalog::{AppCatalog, SystemCatalog};
use crate::uninstall::FfiAppRunState;
use crate::TinySession;

mod error;
mod host;
mod operations;
mod records;
#[cfg(test)]
mod test_support;
#[cfg(test)]
mod tests;

pub use error::FfiCaskError;
pub use records::*;

use host::{BrewHost, SystemBrew};

/// Session-held cask state.
#[derive(Default)]
pub(crate) struct CaskState {
    next_id: u64,
    /// What the last `cask_outdated` listed: the tokens an update may name.
    listing: Option<Vec<OutdatedCask>>,
    preview: Option<StoredPreview>,
    /// Test override for `PREVIEW_TTL`.
    ttl: Option<Duration>,
}

struct StoredPreview {
    id: String,
    action: CaskAction,
    items: Vec<CaskPlanItem>,
    /// Both clocks, as for Clean previews: `Instant` ignores wall-clock
    /// changes, `SystemTime` keeps running while the Mac sleeps.
    expires_at: Instant,
    expires_at_wall: SystemTime,
    /// Kept after use so a replay reports "already used", not "unknown".
    consumed: bool,
}

/// Everything a call touches besides the session: injected in tests.
pub(crate) struct Seams<'a> {
    pub(crate) brew: &'a dyn BrewHost,
    pub(crate) catalog: &'a dyn AppCatalog,
    pub(crate) probe: &'a dyn AppProbe,
}

#[uniffi::export]
impl TinySession {
    /// Whether the Mac has a Homebrew, and which of its `brew.env` settings
    /// override how TinySift runs it. Reads two small files at most; no gate.
    pub fn cask_homebrew_status(&self) -> FfiHomebrewStatus {
        homebrew_status_of(&SystemBrew::new())
    }

    /// The bundled list of apps, each marked with whether an app with its
    /// bundle identifier is on the Mac and whether Homebrew has a record of
    /// it. Holds the gate (it reads every app's identity).
    pub fn cask_catalogue(&self) -> Result<Vec<FfiCaskCatalogueApp>, FfiCaskError> {
        let runner = app_runner();
        let probe = RunnerProbe(runner.as_ref());
        self.catalogue(&Seams {
            brew: &SystemBrew::new(),
            catalog: &SystemCatalog,
            probe: &probe,
        })
    }

    /// The apps Homebrew tracks that have a newer version (the default
    /// `brew outdated`, never greedy), each with whether it is open now.
    /// Replaces the previous list and any preview.
    pub fn cask_outdated(&self) -> Result<Vec<FfiOutdatedCask>, FfiCaskError> {
        let runner = app_runner();
        let probe = RunnerProbe(runner.as_ref());
        self.outdated(&Seams {
            brew: &SystemBrew::new(),
            catalog: &SystemCatalog,
            probe: &probe,
        })
    }

    /// Plans the install of one catalogue app. `Failed` says why not: not in
    /// the catalogue, an app with its bundle identifier already on the Mac
    /// (`AppExists`), Homebrew already lists it (`AlreadyInstalled`), no
    /// Homebrew. A cask that needs an administrator password still gets a
    /// preview, with `refusal` set. Replaces any earlier preview.
    pub fn cask_install_preview(&self, token: String) -> Result<FfiCaskPreview, FfiCaskError> {
        let runner = app_runner();
        let probe = RunnerProbe(runner.as_ref());
        self.install_preview(
            &token,
            &Seams {
                brew: &SystemBrew::new(),
                catalog: &SystemCatalog,
                probe: &probe,
            },
        )
    }

    /// Plans the update of casks from the last `cask_outdated` list, read
    /// again from `brew`. Replaces any earlier preview.
    pub fn cask_update_preview(&self, tokens: Vec<String>) -> Result<FfiCaskPreview, FfiCaskError> {
        let runner = app_runner();
        let probe = RunnerProbe(runner.as_ref());
        self.update_preview(
            &tokens,
            &Seams {
                brew: &SystemBrew::new(),
                catalog: &SystemCatalog,
                probe: &probe,
            },
        )
    }

    /// Runs the selected casks of a preview, in the preview's order, one at a
    /// time. See the module docs for what is refused before the preview is
    /// spent. Every outcome after it is spent is in the returned report: a
    /// failure of one cask never stops the next, a cancel does (but a cask
    /// that has begun installing is let finish first).
    pub fn cask_execute(
        &self,
        preview_id: String,
        selected_tokens: Vec<String>,
        token: Arc<CancellationToken>,
        progress: Arc<dyn CaskProgressListener>,
    ) -> Result<FfiCaskReport, FfiCaskError> {
        let runner = app_runner();
        let probe = RunnerProbe(runner.as_ref());
        self.execute_casks(
            &preview_id,
            &selected_tokens,
            &Seams {
                brew: &SystemBrew::new(),
                catalog: &SystemCatalog,
                probe: &probe,
            },
            &token,
            progress.as_ref(),
        )
    }
}

fn homebrew_status_of(brew: &dyn BrewHost) -> FfiHomebrewStatus {
    let status = brew.status();
    let warnings = match &status {
        HomebrewStatus::Present(path) => brew_env_warnings(path, brew.home().as_deref()),
        HomebrewStatus::Missing => Vec::new(),
    };
    records::homebrew_status(status, &warnings)
}
