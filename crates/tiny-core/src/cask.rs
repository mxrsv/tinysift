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
//!   a flag nor name a third-party tap. Anything else is rejected before a
//!   process spawns.
//! - **Nothing to answer.** `brew` runs with [`BREW_ENV`] and [`BREW_PATH`] in
//!   an otherwise empty environment, a closed stdin and no terminal. A cask
//!   whose installer needs `sudo` is refused ([`Elevation`]), and
//!   `HOMEBREW_NO_SUDO` stops `brew` itself if that check missed one.
//!   `brew.env` files can still override the environment: [`brew_env_warnings`]
//!   names them.
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
mod plan;
mod run;
mod tracked;

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
pub use plan::{CaskAction, CaskPlanItem};
pub use run::{run_item, CaskProgress, ProgressSink, RunOutcome, Stage, RUN_TIMEOUT};
pub use tracked::{list_tracked, TrackedCask};

#[cfg(test)]
mod test_support;
