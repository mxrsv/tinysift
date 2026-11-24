//! An extension's folder on disk: which version folder to read, its
//! `manifest.json` name and version (with `_locales` messages), and its
//! size. Paths are tested with `symlink_metadata`, so symlinks are never
//! followed, and a name read from a file becomes a path only when it has
//! no `..`, root or prefix component.

use std::cmp::Ordering;
use std::fs;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use super::ExtensionSource;

const MANIFEST: &str = "manifest.json";
const LOCALES_DIR: &str = "_locales";
const MESSAGES: &str = "messages.json";
const FALLBACK_LOCALE: &str = "en";
const MESSAGE_PREFIX: &str = "__MSG_";
const MESSAGE_SUFFIX: &str = "__";
const UTF8_BOM: char = '\u{feff}';

/// What the manifest says about the extension.
#[derive(Debug, Default)]
pub(super) struct ManifestInfo {
    /// The name with `__MSG_*__` resolved; `None` when it did not resolve.
    pub name: Option<String>,
    pub version: Option<String>,
}

pub(super) fn is_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file())
}

fn is_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_dir())
}

/// `relative` when it only goes down: no `..`, root or prefix.
fn downward(relative: &str) -> Option<&Path> {
    let path = Path::new(relative);
    let mut components = path.components().peekable();
    components.peek()?;
    components
        .all(|c| matches!(c, Component::Normal(_) | Component::CurDir))
        .then_some(path)
}

/// The profile's own folder for extension `id`, `Extensions/<id>`.
pub(super) fn owned_dir(extensions_dir: &Path, id: &str) -> Option<PathBuf> {
    let mut components = Path::new(id).components();
    let plain = matches!(
        (components.next(), components.next()),
        (Some(Component::Normal(_)), None)
    );
    let dir = extensions_dir.join(id);
    (plain && is_dir(&dir)).then_some(dir)
}

/// The folder holding the extension's manifest: the settings' `path`
/// when it exists (relative to `Extensions`, or absolute for an unpacked
/// extension), else the highest version folder under `Extensions/<id>`.
pub(super) fn locate(
    extensions_dir: &Path,
    id: &str,
    path: Option<&str>,
    source: ExtensionSource,
) -> Option<PathBuf> {
    let from_path = path.and_then(|path| {
        let candidate = if Path::new(path).is_absolute() {
            (source == ExtensionSource::Unpacked).then(|| PathBuf::from(path))?
        } else {
            extensions_dir.join(downward(path)?)
        };
        is_dir(&candidate).then_some(candidate)
    });
    from_path.or_else(|| highest_version(&owned_dir(extensions_dir, id)?))
}

/// The version folder with the highest `1.2.3_4` name.
fn highest_version(dir: &Path) -> Option<PathBuf> {
    fs::read_dir(dir)
        .ok()?
        .flatten()
        .filter(|e| e.file_type().is_ok_and(|t| t.is_dir()))
        .filter_map(|e| {
            let name = e.file_name().into_string().ok()?;
            Some((version_key(&name)?, e.path()))
        })
        .max_by(|a, b| compare_keys(&a.0, &b.0))
        .map(|(_, path)| path)
}

/// `1.2.3_4` as `([1, 2, 3], 4)`; Chromium appends `_<n>` to the version.
/// `None` for a folder that is not a version, such as `Temp`.
fn version_key(name: &str) -> Option<(Vec<u64>, u64)> {
    let (version, install) = match name.rsplit_once('_') {
        Some((version, n)) => (version, n.parse().ok()?),
        None => (name, 0),
    };
    let parts = version
        .split('.')
        .map(|p| p.parse().ok())
        .collect::<Option<Vec<u64>>>()?;
    Some((parts, install))
}

fn compare_keys(a: &(Vec<u64>, u64), b: &(Vec<u64>, u64)) -> Ordering {
    let len = a.0.len().max(b.0.len());
    let part = |v: &[u64], i: usize| v.get(i).copied().unwrap_or(0);
    (0..len)
        .map(|i| part(&a.0, i).cmp(&part(&b.0, i)))
        .find(|o| o.is_ne())
        .unwrap_or(Ordering::Equal)
        .then(a.1.cmp(&b.1))
}

fn read_json(path: &Path) -> Option<Value> {
    if !is_file(path) {
        return None;
    }
    let text = fs::read_to_string(path).ok()?;
    serde_json::from_str(text.trim_start_matches(UTF8_BOM)).ok()
}

/// The name and version in `dir/manifest.json`; `None` without one.
pub(super) fn read(dir: &Path) -> Option<ManifestInfo> {
    let manifest = read_json(&dir.join(MANIFEST))?;
    let text = |key: &str| {
        manifest
            .get(key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
    };
    let name = text("name").and_then(|raw| match message_key(raw) {
        Some(key) => localized(dir, text("default_locale"), key),
        None => Some(raw.to_string()),
    });
    Some(ManifestInfo {
        name,
        version: text("version").map(String::from),
    })
}

/// `appName` for `__MSG_appName__`.
fn message_key(raw: &str) -> Option<&str> {
    raw.strip_prefix(MESSAGE_PREFIX)?
        .strip_suffix(MESSAGE_SUFFIX)
        .filter(|key| !key.is_empty())
}

/// Message `key` from `_locales/<default_locale>/messages.json`, else
/// from `_locales/en`. Keys match without case, as Chromium's do.
fn localized(dir: &Path, default_locale: Option<&str>, key: &str) -> Option<String> {
    let locales = default_locale
        .into_iter()
        .chain((default_locale != Some(FALLBACK_LOCALE)).then_some(FALLBACK_LOCALE));
    locales
        .filter_map(downward)
        .filter_map(|locale| read_json(&dir.join(LOCALES_DIR).join(locale).join(MESSAGES)))
        .find_map(|messages| {
            messages
                .as_object()?
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case(key))?
                .1
                .get("message")?
                .as_str()
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
        })
}

/// Bytes of the regular files under `dir`, not following symlinks.
pub(super) fn size_on_disk(dir: &Path) -> u64 {
    let mut total = 0;
    let mut pending = vec![dir.to_path_buf()];
    while let Some(next) = pending.pop() {
        let Ok(entries) = fs::read_dir(&next) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if meta.file_type().is_dir() {
                pending.push(entry.path());
            } else if meta.file_type().is_file() {
                total += meta.len();
            }
        }
    }
    total
}
