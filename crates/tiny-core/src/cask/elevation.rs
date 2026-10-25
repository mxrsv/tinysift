//! What a cask's artifacts ask of `sudo`, read from `brew info --json=v2`.
//!
//! `brew` runs here with `HOMEBREW_NO_SUDO`, so a cask that needs a password
//! fails before it downloads. Reading the artifacts first lets the preview say
//! so instead of starting a run that cannot finish. The rules mirror
//! Homebrew 7's `Artifact#requires_sudo?` (`pkg`, `keyboard_layout`,
//! `installer`, the install `*_steps`) and the directives an upgrade runs from
//! the old copy's `uninstall` stanza (`abstract_uninstall.rb`). Ruby blocks
//! (`preflight`, `postflight`) are not in the JSON; the `HOMEBREW_NO_SUDO`
//! backstop covers them.
//!
//! `brew info` describes the cask as it is now. The `uninstall` stanza an
//! update runs is the installed copy's, which can differ: the update rule is
//! an approximation, and the backstop catches the rest.

use serde_json::Value;

/// How much of the run needs an administrator password.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Elevation {
    /// Installing needs `sudo`: a `pkg` or `keyboard_layout`, an `installer`
    /// script that says `sudo: true`, or an install step that does.
    pub install: bool,
    /// An `installer manual` entry: `brew` only prints "open <path>" and
    /// installs nothing, so there is nothing to run.
    pub manual_installer: bool,
    /// Replacing an installed copy needs `sudo`: anything that makes installing
    /// need it, or an `uninstall` directive that always uses it (`pkgutil`,
    /// `delete`, `kext`, a `script`/`early_script` with `sudo: true`).
    /// `launchctl` is left out on purpose: it tries `sudo` and carries on
    /// without it.
    pub update: bool,
}

impl Elevation {
    pub(super) fn of(artifacts: &[Value]) -> Self {
        let mut found = Self::default();
        let mut uninstall_needs_root = false;
        for (key, value) in artifacts.iter().filter_map(Value::as_object).flatten() {
            match key.as_str() {
                "pkg" | "keyboard_layout" => found.install = true,
                "installer" => {
                    found.manual_installer |= entries(value).any(|e| e.get("manual").is_some());
                    found.install |= says_sudo(value);
                }
                "uninstall" => uninstall_needs_root |= entries(value).any(always_root),
                key if key.starts_with("uninstall_") && key.ends_with("_steps") => {
                    uninstall_needs_root |= says_sudo(value)
                }
                key if key.ends_with("_steps") => found.install |= says_sudo(value),
                _ => {}
            }
        }
        found.update = found.install || uninstall_needs_root;
        found
    }
}

/// The objects of an artifact's value, which is an array of them.
fn entries(value: &Value) -> impl Iterator<Item = &serde_json::Map<String, Value>> {
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_object)
}

/// One `uninstall` directive block that uses `sudo` whatever the paths are.
fn always_root(directives: &serde_json::Map<String, Value>) -> bool {
    directives.iter().any(|(key, value)| match key.as_str() {
        "pkgutil" | "delete" | "kext" => true,
        "script" | "early_script" => says_sudo(value),
        _ => false,
    })
}

/// A `sudo: true` anywhere inside `value`, or a step that deletes a keychain
/// certificate (which Homebrew runs as root).
fn says_sudo(value: &Value) -> bool {
    match value {
        Value::Object(map) => map.iter().any(|(key, value)| {
            matches!((key.as_str(), value), ("sudo", Value::Bool(true)))
                || matches!((key.as_str(), value), ("type", Value::String(kind))
                    if kind == "delete_keychain_certificate")
                || says_sudo(value)
        }),
        Value::Array(items) => items.iter().any(says_sudo),
        _ => false,
    }
}

#[cfg(test)]
mod tests;
