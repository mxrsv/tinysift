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
//! - **No prompts, no administrator.** `brew` runs with [`BREW_ENV`] and a
//!   closed stdin. A cask whose installer needs `sudo` is refused
//!   ([`CaskInfo::needs_admin`]), and `HOMEBREW_NO_SUDO` stops `brew` itself
//!   if that check missed one.
//! - **Previews are single-use.** Core builds and runs plan items; the FFI
//!   session owns the preview's id, expiry, single use and the busy gate.
//!
//! Tests inject [`CommandRunner`](crate::runner::CommandRunner) fakes; none
//! runs a real `brew`.

mod brew;
mod catalogue;
mod failure;
mod info;

pub use brew::{brew_lookup, locate_brew, Brew, HomebrewStatus, BREW_ENV};
pub use catalogue::{
    catalogue_entry, catalogue_with_presence, CatalogueApp, CatalogueEntry, Group, CATALOGUE,
};
pub use failure::CaskFailure;
pub use info::{read_cask_info, valid_token, CaskInfo};

#[cfg(test)]
mod test_support;
