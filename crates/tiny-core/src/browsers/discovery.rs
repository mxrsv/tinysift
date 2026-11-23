//! Finds the installed browsers and their profiles under a home folder.
//! Reads `Local State` and `profiles.ini` only; never opens a profile's
//! databases. Paths are tested with `symlink_metadata`, so a symlinked
//! folder is not followed, and a name read from a file is used as a path
//! only when it is one plain component.

use std::collections::HashSet;
use std::fs;
use std::io;
use std::path::{Component, Path, PathBuf};

use serde_json::{Map, Value};

use super::{
    Browser, BrowserFamily, BrowserKind, BrowserProfile, DiscoveryIssue, DiscoveryIssueKind,
    ProfileAvatar,
};

const LOCAL_STATE: &str = "Local State";
/// A file every Chromium profile folder holds, also at the data root for
/// a root-level profile.
const PREFERENCES: &str = "Preferences";
const PROFILES_INI: &str = "profiles.ini";
const DEFAULT_PROFILE: &str = "Default";
const APPLICATION_SUPPORT: &str = "Library/Application Support";
const CACHES: &str = "Library/Caches";
const SAFARI_CONTAINER: &str = "Library/Containers/com.apple.Safari";
const SAFARI_CONTAINER_DATA: &str = "Data/Library/Safari";
const SAFARI_CACHE: &str = "Library/Caches/com.apple.Safari";
const AVATAR_PREFIX: &str = "IDR_PROFILE_AVATAR_";

/// Every installed browser under `home`, in [`BrowserKind::ALL`] order.
/// Never fails: a browser whose profile list cannot be read is returned
/// with no profiles and a [`DiscoveryIssue`].
pub fn discover(home: &Path) -> Vec<Browser> {
    BrowserKind::ALL
        .iter()
        .filter_map(|&kind| match kind.family() {
            BrowserFamily::Chromium => chromium(home, kind),
            BrowserFamily::WebKit => safari(home),
            BrowserFamily::Gecko => firefox(home),
        })
        .collect()
}

fn is_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_dir())
}

fn is_file(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_file())
}

/// `name` as one plain path component, or `None` for `..`, `/x`, `a/b`.
fn plain_component(name: &str) -> Option<&str> {
    let mut components = Path::new(name).components();
    match (components.next(), components.next()) {
        (Some(Component::Normal(_)), None) => Some(name),
        _ => None,
    }
}

fn non_empty(value: Option<&Value>) -> Option<String> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
}

fn empty_browser(kind: BrowserKind, data_root: PathBuf) -> Browser {
    Browser {
        kind,
        family: kind.family(),
        name: kind.display_name().to_string(),
        data_root,
        container_dir: None,
        bundle_id: kind.bundle_id().to_string(),
        helper_bundle_prefixes: kind
            .helper_bundle_prefixes()
            .iter()
            .map(|p| p.to_string())
            .collect(),
        app_names: kind.app_names().iter().map(|n| n.to_string()).collect(),
        profiles: Vec::new(),
        issue: None,
    }
}

/// The profile's cache folder: its path below `Application Support`
/// moved to `Caches`, when that folder exists.
fn cache_dir(home: &Path, data_dir: &Path) -> Option<PathBuf> {
    let relative = data_dir.strip_prefix(home.join(APPLICATION_SUPPORT)).ok()?;
    let cache = home.join(CACHES).join(relative);
    is_dir(&cache).then_some(cache)
}

fn read_issue(path: &Path, error: &io::Error) -> DiscoveryIssue {
    let kind = if error.kind() == io::ErrorKind::PermissionDenied {
        DiscoveryIssueKind::PermissionDenied
    } else {
        DiscoveryIssueKind::Unreadable
    };
    DiscoveryIssue {
        kind,
        path: path.to_path_buf(),
        detail: error.to_string(),
    }
}

fn malformed(path: &Path, detail: impl Into<String>) -> DiscoveryIssue {
    DiscoveryIssue {
        kind: DiscoveryIssueKind::Malformed,
        path: path.to_path_buf(),
        detail: detail.into(),
    }
}

// Chromium ---------------------------------------------------------------

/// A Chromium browser counts when its data root holds `Local State` or a
/// profile folder; a root with only `NativeMessagingHosts` does not.
fn chromium(home: &Path, kind: BrowserKind) -> Option<Browser> {
    let root = home.join(kind.data_root());
    if !is_dir(&root) {
        return None;
    }
    let local_state = root.join(LOCAL_STATE);
    if !is_file(&local_state) && !holds_profile_folder(&root, kind) {
        return None;
    }
    let mut browser = empty_browser(kind, root.clone());
    match read_info_cache(&local_state) {
        Ok(state) => browser.profiles = chromium_profiles(home, kind, &root, &state),
        Err(issue) => browser.issue = Some(issue),
    }
    Some(browser)
}

fn holds_profile_folder(root: &Path, kind: BrowserKind) -> bool {
    if kind.allows_root_profile() && is_file(&root.join(PREFERENCES)) {
        return true;
    }
    fs::read_dir(root).is_ok_and(|entries| {
        entries
            .flatten()
            .any(|entry| is_dir(&entry.path()) && is_file(&entry.path().join(PREFERENCES)))
    })
}

/// `profile.info_cache` and `profile.profiles_order` from `Local State`.
struct ChromiumState {
    info_cache: Map<String, Value>,
    order: Vec<String>,
}

fn read_info_cache(path: &Path) -> Result<ChromiumState, DiscoveryIssue> {
    let bytes = fs::read(path).map_err(|e| read_issue(path, &e))?;
    let json: Value = serde_json::from_slice(&bytes).map_err(|e| malformed(path, e.to_string()))?;
    let Some(top) = json.as_object() else {
        return Err(malformed(path, "not a JSON object"));
    };
    let profile = match top.get("profile") {
        None => None,
        Some(Value::Object(profile)) => Some(profile),
        Some(_) => return Err(malformed(path, "`profile` is not an object")),
    };
    let info_cache = match profile.and_then(|p| p.get("info_cache")) {
        None => Map::new(),
        Some(Value::Object(cache)) => cache.clone(),
        Some(_) => return Err(malformed(path, "`profile.info_cache` is not an object")),
    };
    let order = profile
        .and_then(|p| p.get("profiles_order"))
        .and_then(Value::as_array)
        .map(|list| {
            list.iter()
                .filter_map(Value::as_str)
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    Ok(ChromiumState { info_cache, order })
}

/// Profiles in `profiles_order`, then the rest by folder name. A listed
/// folder missing on disk is skipped; a folder not listed is ignored,
/// except Opera's root-level profile.
fn chromium_profiles(
    home: &Path,
    kind: BrowserKind,
    root: &Path,
    state: &ChromiumState,
) -> Vec<BrowserProfile> {
    let mut keys: Vec<&String> = Vec::new();
    for key in &state.order {
        if let Some((key, _)) = state.info_cache.get_key_value(key) {
            if !keys.contains(&key) {
                keys.push(key);
            }
        }
    }
    let mut rest: Vec<&String> = state
        .info_cache
        .keys()
        .filter(|k| !keys.contains(k))
        .collect();
    rest.sort();
    keys.extend(rest);

    let root_holds_profile = kind.allows_root_profile() && is_file(&root.join(PREFERENCES));
    let mut used: HashSet<PathBuf> = HashSet::new();
    let mut profiles = Vec::new();
    for key in keys {
        let Some(entry) = state.info_cache.get(key).and_then(Value::as_object) else {
            continue;
        };
        let Some(folder) = plain_component(key) else {
            continue;
        };
        let dir = root.join(folder);
        let data_dir = if is_dir(&dir) {
            dir
        } else if root_holds_profile && folder == DEFAULT_PROFILE {
            root.to_path_buf()
        } else {
            continue;
        };
        if !used.insert(data_dir.clone()) {
            continue;
        }
        profiles.push(chromium_profile(home, key, entry, data_dir));
    }
    if root_holds_profile && !used.contains(root) {
        profiles.push(BrowserProfile {
            id: String::new(),
            name: kind.display_name().to_string(),
            user_name: None,
            avatar: ProfileAvatar::default(),
            highlight_color: None,
            data_dir: root.to_path_buf(),
            cache_dir: cache_dir(home, root),
        });
    }
    profiles
}

fn chromium_profile(
    home: &Path,
    key: &str,
    entry: &Map<String, Value>,
    data_dir: PathBuf,
) -> BrowserProfile {
    let icon = non_empty(entry.get("avatar_icon"));
    let icon_index = icon.as_deref().and_then(avatar_index);
    let account_picture = non_empty(entry.get("gaia_picture_file_name"))
        .as_deref()
        .and_then(plain_component)
        .map(|file| data_dir.join(file))
        .filter(|path| is_file(path));
    BrowserProfile {
        id: key.to_string(),
        name: non_empty(entry.get("name")).unwrap_or_else(|| key.to_string()),
        user_name: non_empty(entry.get("user_name")),
        avatar: ProfileAvatar {
            icon,
            icon_index,
            uses_account_picture: entry
                .get("use_gaia_picture")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            account_picture,
        },
        highlight_color: entry
            .get("profile_highlight_color")
            .and_then(Value::as_i64)
            .and_then(argb),
        cache_dir: cache_dir(home, &data_dir),
        data_dir,
    }
}

/// `chrome://theme/IDR_PROFILE_AVATAR_26` -> 26.
fn avatar_index(icon: &str) -> Option<u32> {
    let (_, digits) = icon.rsplit_once(AVATAR_PREFIX)?;
    digits.parse().ok()
}

/// Chromium stores an `SkColor` as a signed or unsigned 32-bit integer.
fn argb(value: i64) -> Option<u32> {
    i32::try_from(value)
        .map(|v| v as u32)
        .or_else(|_| u32::try_from(value))
        .ok()
}

// Safari -----------------------------------------------------------------

/// Safari is reported when `~/Library/Safari` or its container exists,
/// with one profile named "Safari".
fn safari(home: &Path) -> Option<Browser> {
    let kind = BrowserKind::Safari;
    let data_root = home.join(kind.data_root());
    let container = home.join(SAFARI_CONTAINER);
    let has_container = is_dir(&container);
    if !is_dir(&data_root) && !has_container {
        return None;
    }
    let data_dir = if is_dir(&data_root) || !has_container {
        data_root.clone()
    } else {
        container.join(SAFARI_CONTAINER_DATA)
    };
    let cache = home.join(SAFARI_CACHE);
    let mut browser = empty_browser(kind, data_root);
    browser.container_dir = has_container.then_some(container);
    browser.profiles = vec![BrowserProfile {
        id: String::new(),
        name: kind.display_name().to_string(),
        user_name: None,
        avatar: ProfileAvatar::default(),
        highlight_color: None,
        data_dir,
        cache_dir: is_dir(&cache).then_some(cache),
    }];
    Some(browser)
}

// Firefox ----------------------------------------------------------------

fn firefox(home: &Path) -> Option<Browser> {
    let kind = BrowserKind::Firefox;
    let root = home.join(kind.data_root());
    let ini = root.join(PROFILES_INI);
    if !is_file(&ini) {
        return None;
    }
    let mut browser = empty_browser(kind, root.clone());
    match read_profiles_ini(&ini) {
        Ok(sections) => browser.profiles = firefox_profiles(home, &root, &sections),
        Err(issue) => browser.issue = Some(issue),
    }
    Some(browser)
}

/// One `[ProfileN]` section of `profiles.ini`.
#[derive(Default)]
struct IniProfile {
    name: Option<String>,
    path: Option<String>,
    is_relative: Option<String>,
}

fn read_profiles_ini(path: &Path) -> Result<Vec<IniProfile>, DiscoveryIssue> {
    let text = fs::read_to_string(path).map_err(|e| {
        if e.kind() == io::ErrorKind::InvalidData {
            malformed(path, "not UTF-8 text")
        } else {
            read_issue(path, &e)
        }
    })?;
    parse_profiles_ini(&text).map_err(|line| malformed(path, format!("line {line} is not INI")))
}

/// The `[ProfileN]` sections in file order; `Err` holds the 1-based
/// number of the first line that is no section, key, comment or blank.
fn parse_profiles_ini(text: &str) -> Result<Vec<IniProfile>, usize> {
    let mut profiles = Vec::new();
    let mut current: Option<IniProfile> = None;
    for (index, raw) in text.lines().enumerate() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if let Some(section) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            profiles.extend(current.take());
            let is_profile = section
                .strip_prefix("Profile")
                .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()));
            current = is_profile.then(IniProfile::default);
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            return Err(index + 1);
        };
        if let Some(profile) = current.as_mut() {
            let value = Some(value.trim().to_string());
            match key.trim() {
                "Name" => profile.name = value,
                "Path" => profile.path = value,
                "IsRelative" => profile.is_relative = value,
                _ => {}
            }
        }
    }
    profiles.extend(current);
    Ok(profiles)
}

fn firefox_profiles(home: &Path, root: &Path, sections: &[IniProfile]) -> Vec<BrowserProfile> {
    let mut used: HashSet<PathBuf> = HashSet::new();
    let mut profiles = Vec::new();
    for section in sections {
        let Some(path) = section.path.as_deref().filter(|p| !p.is_empty()) else {
            continue;
        };
        let Some(data_dir) = firefox_profile_dir(root, path, section.is_relative.as_deref()) else {
            continue;
        };
        if !is_dir(&data_dir) || !used.insert(data_dir.clone()) {
            continue;
        }
        let fallback = data_dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string());
        profiles.push(BrowserProfile {
            id: path.to_string(),
            name: section
                .name
                .clone()
                .filter(|n| !n.is_empty())
                .unwrap_or(fallback),
            user_name: None,
            avatar: ProfileAvatar::default(),
            highlight_color: None,
            cache_dir: cache_dir(home, &data_dir),
            data_dir,
        });
    }
    profiles
}

/// `Path` resolved against the Firefox folder unless `IsRelative=0`. A
/// relative path that climbs out (`..`) or an absolute one marked relative
/// is refused.
fn firefox_profile_dir(root: &Path, path: &str, is_relative: Option<&str>) -> Option<PathBuf> {
    let candidate = Path::new(path);
    if is_relative == Some("0") {
        return candidate.is_absolute().then(|| candidate.to_path_buf());
    }
    candidate
        .components()
        .all(|c| matches!(c, Component::Normal(_)))
        .then(|| root.join(candidate))
}
