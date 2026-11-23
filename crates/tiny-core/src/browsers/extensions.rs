//! Extensions the user installed in a Chromium profile, read-only.
//!
//! [`list`] reads `extensions.settings` from the profile's
//! `Secure Preferences` (falling back to `Preferences`), keeps the
//! extensions the user added (from the web store or unpacked), and reads
//! each one's name and version from its `manifest.json`. Nothing here
//! writes: `extensions.settings` is HMAC-protected and deleting an
//! extension's folder only makes the browser re-download it, so removal
//! is the user's click in `chrome://extensions/?id=<id>`.
//!
//! Chromium keeps no last-used time for an extension, so nothing here
//! says one is unused; [`Extension::stale`] only says it has not been
//! updated in a year.

mod manifest;

#[cfg(test)]
mod tests;

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::{BrowserFamily, BrowserKind, BrowserProfile, DiscoveryIssue, DiscoveryIssueKind};

const SECURE_PREFERENCES: &str = "Secure Preferences";
const PREFERENCES: &str = "Preferences";
const EXTENSIONS_DIR: &str = "Extensions";
/// Chromium's `ManifestLocation::kInternal`: installed by the user,
/// normally from the web store.
const LOCATION_INTERNAL: i64 = 1;
/// `ManifestLocation::kUnpacked`: loaded unpacked in developer mode.
const LOCATION_UNPACKED: i64 = 4;
/// Seconds from 1601-01-01 (Chromium's epoch) to 1970-01-01 UTC.
const CHROMIUM_EPOCH_OFFSET_SECS: i64 = 11_644_473_600;
const MICROS_PER_SEC: i64 = 1_000_000;
/// An extension not updated for longer than this is flagged stale.
pub const STALE_AFTER_SECS: i64 = 365 * 86_400;

/// The extensions of one profile.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProfileExtensions {
    /// User-installed extensions, by name then id.
    pub extensions: Vec<Extension>,
    /// Why the preferences could not be read; `extensions` is then empty.
    pub issue: Option<DiscoveryIssue>,
}

/// How the user added an extension.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExtensionSource {
    /// Installed by the user (`location` 1), normally from the web store.
    Installed,
    /// Loaded unpacked in developer mode (`location` 4); its folder is
    /// wherever the user keeps it, outside the profile.
    Unpacked,
}

/// One user-installed extension.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Extension {
    /// The 32-letter extension id, as in `chrome://extensions/?id=<id>`.
    pub id: String,
    /// The manifest's name with `__MSG_*__` resolved, or the id when
    /// neither the manifest nor its messages give one.
    pub name: String,
    /// The manifest's `version`; `None` without a readable manifest.
    pub version: Option<String>,
    pub source: ExtensionSource,
    pub from_webstore: bool,
    /// Turned off: `disable_reasons` is non-zero or non-empty.
    pub disabled: bool,
    /// Last updated (or, never updated, installed) more than
    /// [`STALE_AFTER_SECS`] before `now`. Never read as "unused".
    pub stale: bool,
    /// `first_install_time`, Unix seconds.
    pub installed_at: Option<i64>,
    /// `last_update_time`, Unix seconds.
    pub updated_at: Option<i64>,
    /// The version folder the manifest was read from.
    pub dir: Option<PathBuf>,
    /// Bytes of the files under the extension's folder in the profile
    /// (every version kept), or under `dir` for an unpacked one.
    /// Symlinks are not followed.
    pub size_bytes: u64,
}

/// The user-installed extensions of `profile`, flagged against
/// `now_unix` (Unix seconds). A non-Chromium browser has none listed here
/// (Safari's live inside apps). Never fails: unreadable or malformed
/// preferences give an empty list and an issue.
pub fn list(kind: BrowserKind, profile: &BrowserProfile, now_unix: i64) -> ProfileExtensions {
    if kind.family() != BrowserFamily::Chromium {
        return ProfileExtensions::default();
    }
    list_in(&profile.data_dir, now_unix)
}

/// [`list`] for a Chromium profile folder.
pub fn list_in(profile_dir: &Path, now_unix: i64) -> ProfileExtensions {
    let settings = match read_settings(profile_dir) {
        Ok(settings) => settings,
        Err(issue) => {
            return ProfileExtensions {
                extensions: Vec::new(),
                issue: Some(issue),
            }
        }
    };
    let extensions_dir = profile_dir.join(EXTENSIONS_DIR);
    let mut extensions: Vec<Extension> = settings
        .iter()
        .filter_map(|(id, entry)| extension(&extensions_dir, id, entry, now_unix))
        .collect();
    extensions.sort_by(|a, b| (a.name.to_lowercase(), &a.id).cmp(&(b.name.to_lowercase(), &b.id)));
    ProfileExtensions {
        extensions,
        issue: None,
    }
}

/// `extensions.settings` from `Secure Preferences`, else `Preferences`.
/// When neither holds one, a file that exists but failed is reported
/// (macOS Chromium keeps the list in `Secure Preferences` only, so a
/// broken one must not read as "no extensions"), then a missing
/// `Secure Preferences` when both are missing; otherwise the list is
/// empty.
fn read_settings(profile_dir: &Path) -> Result<Map<String, Value>, DiscoveryIssue> {
    let mut failed = None;
    let mut missing = None;
    let mut parsed = false;
    for name in [SECURE_PREFERENCES, PREFERENCES] {
        let path = profile_dir.join(name);
        if !manifest::is_file(&path) {
            missing.get_or_insert_with(|| {
                issue(
                    DiscoveryIssueKind::Unreadable,
                    &path,
                    "missing or not a regular file",
                )
            });
            continue;
        }
        match read_prefs(&path) {
            Ok(Some(settings)) => return Ok(settings),
            Ok(None) => parsed = true,
            Err(issue) => {
                failed.get_or_insert(issue);
            }
        }
    }
    match (failed, parsed) {
        (Some(issue), _) => Err(issue),
        (None, false) => missing.map_or_else(|| Ok(Map::new()), Err),
        (None, true) => Ok(Map::new()),
    }
}

/// `Some(settings)` when the file holds `extensions.settings`, `None`
/// when it parses without one.
fn read_prefs(path: &Path) -> Result<Option<Map<String, Value>>, DiscoveryIssue> {
    let text = fs::read_to_string(path).map_err(|e| read_issue(path, &e))?;
    let prefs: Value = serde_json::from_str(&text)
        .map_err(|e| issue(DiscoveryIssueKind::Malformed, path, e.to_string()))?;
    if !prefs.is_object() {
        return Err(issue(
            DiscoveryIssueKind::Malformed,
            path,
            "not a JSON object",
        ));
    }
    Ok(prefs
        .pointer("/extensions/settings")
        .and_then(Value::as_object)
        .cloned())
}

fn read_issue(path: &Path, error: &io::Error) -> DiscoveryIssue {
    let kind = if error.kind() == io::ErrorKind::PermissionDenied {
        DiscoveryIssueKind::PermissionDenied
    } else {
        DiscoveryIssueKind::Unreadable
    };
    issue(kind, path, error.to_string())
}

fn issue(kind: DiscoveryIssueKind, path: &Path, detail: impl Into<String>) -> DiscoveryIssue {
    DiscoveryIssue {
        kind,
        path: path.to_path_buf(),
        detail: detail.into(),
    }
}

/// One settings entry as an [`Extension`], or `None` when the browser,
/// an administrator or another app put it there.
fn extension(extensions_dir: &Path, id: &str, entry: &Value, now_unix: i64) -> Option<Extension> {
    let entry = entry.as_object()?;
    let source = match entry.get("location").and_then(Value::as_i64)? {
        LOCATION_INTERNAL => ExtensionSource::Installed,
        LOCATION_UNPACKED => ExtensionSource::Unpacked,
        _ => return None,
    };
    let flag = |key: &str| entry.get(key).and_then(Value::as_bool).unwrap_or(false);
    if flag("was_installed_by_default") || flag("was_installed_by_oem") {
        return None;
    }
    let installed_at = entry.get("first_install_time").and_then(chromium_time);
    let updated_at = entry.get("last_update_time").and_then(chromium_time);
    let stale = updated_at
        .or(installed_at)
        .is_some_and(|at| now_unix - at > STALE_AFTER_SECS);
    let path = entry.get("path").and_then(Value::as_str);
    let found = manifest::locate(extensions_dir, id, path, source);
    let info = found.as_deref().and_then(manifest::read);
    let size_bytes = match source {
        ExtensionSource::Installed => manifest::owned_dir(extensions_dir, id),
        ExtensionSource::Unpacked => found.clone(),
    }
    .map_or(0, |dir| manifest::size_on_disk(&dir));
    Some(Extension {
        id: id.to_string(),
        name: info
            .as_ref()
            .and_then(|i| i.name.clone())
            .unwrap_or_else(|| id.to_string()),
        version: info.and_then(|i| i.version),
        source,
        from_webstore: flag("from_webstore"),
        disabled: is_disabled(entry),
        stale,
        installed_at,
        updated_at,
        dir: found,
        size_bytes,
    })
}

/// `disable_reasons` is a bitmask in older Chromium and a list of reason
/// codes in newer ones; either non-empty means off. Without it, the old
/// `state` field (0 = disabled) decides.
fn is_disabled(entry: &Map<String, Value>) -> bool {
    match entry.get("disable_reasons") {
        Some(Value::Number(n)) => n.as_i64() != Some(0),
        Some(Value::Array(reasons)) => !reasons.is_empty(),
        Some(_) => false,
        None => entry.get("state").and_then(Value::as_i64) == Some(0),
    }
}

/// A Chromium time (microseconds since 1601-01-01 UTC, as a decimal
/// string or a number) in Unix seconds; `None` for 0 or junk.
fn chromium_time(value: &Value) -> Option<i64> {
    let micros = match value {
        Value::String(s) => s.trim().parse::<i64>().ok()?,
        Value::Number(n) => n.as_i64()?,
        _ => return None,
    };
    (micros > 0).then(|| micros / MICROS_PER_SEC - CHROMIUM_EPOCH_OFFSET_SECS)
}
