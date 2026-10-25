//! Installing and updating desktop apps through the user's own Homebrew.
//!
//! The contract, which the FFI session ([`tiny-ffi`'s `cask`](../../tiny-ffi/src/cask.rs))
//! relies on and tests:
//!
//! - **Homebrew is the user's.** [`locate_brew`] finds an existing `brew` and
//!   nothing here installs Homebrew or downloads an app: `brew` does the
//!   download, and only after the user has seen the version, the download host
//!   and the exact command in a preview.
//! - **Argument lists only.** `brew` is spawned from an absolute path with
//!   fixed arguments (`install --cask <token>`), never through a shell.
//! - **Token allow-list.** A token must be in the bundled [`CATALOGUE`] to be
//!   installed, or in the last `brew outdated` listing to be updated, and it
//!   must look like a bare cask token ([`valid_token`]) so it can neither add
//!   a flag nor name a tap, which `brew` would then trust or clone. Anything
//!   else is rejected before a process spawns. `brew` itself decides which
//!   cask a bare token means, so an installed cask from a third-party tap is
//!   handled as `brew` resolves it, and one `brew info` refuses leaves that
//!   cask out of the update list.
//! - **Nothing to answer.** `brew` runs with [`BREW_ENV`] and [`BREW_PATH`] in
//!   an otherwise empty environment, a closed stdin and no terminal. A cask
//!   whose installer needs `sudo` is refused ([`Elevation`]), and
//!   `HOMEBREW_NO_SUDO` stops `brew` itself if that check missed one.
//!   `brew.env` files can still override the environment: [`brew_env_warnings`]
//!   names them.
//! - **Only real updates.** An update is offered, planned and run only for a
//!   cask in the default `brew outdated` (never `--greedy`) whose app is not
//!   already at or past the cask's version; the rule that orders versions is
//!   in `version.rs`. Updates are run with `--no-quit`: an open app is
//!   refused, never closed for the user.
//! - **Previews are single-use.** Core builds and runs plan items; the FFI
//!   session owns the preview's id, expiry, single use and the busy gate.
//!
//! Tests inject [`CommandRunner`](crate::runner::CommandRunner) fakes; none
//! runs a real `brew`.

mod brew;
mod brew_env;
mod catalogue;
mod elevation;
mod failure;
mod info;
mod install;
mod outdated;
mod plan;
mod run;
mod tracked;
mod update;
mod version;

pub use brew::{brew_lookup, locate_brew, Brew, HomebrewStatus, BREW_ENV, BREW_PATH};
pub use brew_env::{brew_env_warnings, BrewEnvWarning};
pub use catalogue::{
    catalogue_entry, catalogue_with_presence, installed_copy, CatalogueApp, CatalogueEntry, Group,
    CATALOGUE,
};
pub use elevation::Elevation;
pub use failure::CaskFailure;
pub use info::{display_version, read_cask_info, valid_token, CaskInfo};
pub use install::{plan_install, run_install};
pub use outdated::{list_outdated, still_outdated, OutdatedCask};
pub use plan::{CaskAction, CaskPlanItem};
pub use run::{run_item, CaskProgress, ProgressSink, RunOutcome, Stage, RUN_TIMEOUT};
pub use tracked::{list_tracked, TrackedCask};
pub use update::{
    check_apps_closed, plan_updates, run_state, run_updates, ExecutableOf, RunState,
    UpdateRequestError,
};

#[cfg(test)]
mod test_support;
