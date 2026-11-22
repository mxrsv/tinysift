use std::collections::HashSet;
use std::path::{Path, PathBuf};

use crate::browsers::{discover, Browser, BrowserFamily, BrowserKind};
use crate::error::Result;

use super::{execute_per_item, root_as_item, CleanProvider};
use crate::clean::fs_safe::{is_dir_safe, list_children};
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, Evidence, ExecAction, ExecReport, RiskLevel};

const ID: &str = "browser-caches";
const LABEL: &str = "Browser caches";

/// Cache-only folders. NEVER include Cookies, Login Data, Preferences,
/// Secure Preferences, History, Local State, Extensions, Local Storage,
/// IndexedDB or `Service Worker/CacheStorage` (`browser-offline-data`'s):
/// those carry user state that is not cache. Tests below pin this.
///
/// Inside a Chromium profile's cache folder
/// (`~/Library/Caches/<vendor>/<browser>/<profile>`).
const CHROMIUM_CACHE_FOLDERS: &[&str] = &["Cache", "Code Cache", "Storage"];
/// Inside a Chromium profile's data folder (`Application Support`).
const CHROMIUM_DATA_FOLDERS: &[&str] = &[
    "Cache",
    "Code Cache",
    "GPUCache",
    "Service Worker/ScriptCache",
];
const SAFARI_CACHE: &str = "Library/Caches/com.apple.Safari";
/// Firefox keeps its disk cache at `<profile>/cache2`, the profile under
/// either root (`Caches` on current versions).
const FIREFOX_CACHE: &str = "cache2";
const FIREFOX_PROFILES_ROOTS: &[&str] = &[
    "Library/Application Support/Firefox/Profiles",
    "Library/Caches/Firefox/Profiles",
];
const APPLICATION_SUPPORT: &str = "Library/Application Support";
const CACHES: &str = "Library/Caches";

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// One folder a browser category offers, with the browser and profile it
/// belongs to.
pub(super) struct ProfileFolder {
    pub path: PathBuf,
    pub kind: BrowserKind,
    pub profile_id: String,
    pub profile_name: String,
}

/// Where `kind` keeps its disk caches, relative to the home folder: its
/// data root moved from `Application Support` to `Caches` (Chromium and
/// Firefox), or Safari's `com.apple.Safari`.
fn cache_root(kind: BrowserKind) -> String {
    match kind.family() {
        BrowserFamily::WebKit => SAFARI_CACHE.to_string(),
        _ => match kind.data_root().strip_prefix(APPLICATION_SUPPORT) {
            Some(rest) => format!("{CACHES}{rest}"),
            None => kind.data_root().to_string(),
        },
    }
}

/// The browser whose folders hold `path`, by its data root or cache root.
pub(super) fn browser_of(home: &Path, path: &Path) -> Option<BrowserKind> {
    BrowserKind::ALL.into_iter().find(|&kind| {
        path.starts_with(home.join(kind.data_root()))
            || path.starts_with(home.join(cache_root(kind)))
    })
}

/// Process names that must not run while `path` is acted on: its
/// browser's, or every browser's for a path no browser claims.
pub(super) fn gate_apps(path: &Path) -> Vec<String> {
    let names: Vec<&str> = match home().and_then(|h| browser_of(&h, path)) {
        Some(kind) => kind.app_names().to_vec(),
        None => BrowserKind::ALL
            .iter()
            .flat_map(|kind| kind.app_names().iter().copied())
            .collect(),
    };
    names.into_iter().map(String::from).collect()
}

/// The `~/Library/Caches` children that hold an installed browser's cache
/// root (`Google`, `BraveSoftware`, `Firefox`, ...). `browser-caches` lists
/// what is inside them, so `user-caches` never offers them whole.
pub(super) fn covered_caches_children(home: &Path) -> Vec<PathBuf> {
    let caches = home.join(CACHES);
    let mut out: Vec<PathBuf> = Vec::new();
    for browser in discover(home) {
        let root = home.join(cache_root(browser.kind));
        let Ok(rest) = root.strip_prefix(&caches) else {
            continue;
        };
        if let Some(first) = rest.components().next() {
            let child = caches.join(first);
            if !out.contains(&child) {
                out.push(child);
            }
        }
    }
    out
}

/// Items for `folders`, leaving out the folders of a running browser.
pub(super) fn offer(
    ctx: &ScanContext<'_>,
    folders: Vec<ProfileFolder>,
    id: &str,
    label: &str,
) -> Vec<CleanItem> {
    let mut items = Vec::new();
    for folder in folders {
        if folder.kind.app_names().iter().any(|a| ctx.known_running(a)) {
            continue;
        }
        let evidence = Evidence::BrowserProfile {
            browser: folder.kind.display_name().to_string(),
            browser_kind: folder.kind,
            profile_id: folder.profile_id,
            profile_name: folder.profile_name,
        };
        items.extend(
            root_as_item(ctx, &folder.path, id, label, RiskLevel::Review)
                .into_iter()
                .map(|item| CleanItem {
                    evidence: vec![evidence.clone()],
                    ..item
                }),
        );
    }
    items
}

/// `base/relative` when it and every folder on the way are real folders,
/// so a symlinked `Service Worker` is not followed.
pub(super) fn real_folder(base: &Path, relative: &str) -> Option<PathBuf> {
    let mut path = base.to_path_buf();
    for part in relative.split('/') {
        path.push(part);
        if !is_dir_safe(&path) {
            return None;
        }
    }
    Some(path)
}

/// Every cache folder of every installed browser and profile under `home`.
pub(super) fn cache_folders(ctx: &ScanContext<'_>, home: &Path) -> Vec<ProfileFolder> {
    let browsers = discover(home);
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut out = Vec::new();
    let mut push = |path: Option<PathBuf>, kind, id: &str, name: &str| {
        let Some(path) = path else { return };
        if seen.insert(path.clone()) {
            out.push(ProfileFolder {
                path,
                kind,
                profile_id: id.to_string(),
                profile_name: name.to_string(),
            });
        }
    };
    for browser in browsers
        .iter()
        .filter(|b| b.family == BrowserFamily::Chromium)
    {
        for profile in &browser.profiles {
            let cached = profile.cache_dir.iter().flat_map(|dir| {
                CHROMIUM_CACHE_FOLDERS
                    .iter()
                    .map(move |folder| real_folder(dir, folder))
            });
            let data = CHROMIUM_DATA_FOLDERS
                .iter()
                .map(|folder| real_folder(&profile.data_dir, folder));
            for path in cached.chain(data) {
                push(path, browser.kind, &profile.id, &profile.name);
            }
        }
    }
    let safari = BrowserKind::Safari;
    push(
        real_folder(home, SAFARI_CACHE),
        safari,
        "",
        safari.display_name(),
    );
    for cache2 in FIREFOX_PROFILES_ROOTS
        .iter()
        .flat_map(|root| firefox_cache_dirs(ctx, &home.join(root)))
    {
        let (id, name) = firefox_profile(&browsers, &cache2);
        push(Some(cache2), BrowserKind::Firefox, &id, &name);
    }
    out
}

/// The Firefox profile a `cache2` folder belongs to: the discovered one
/// whose data or cache folder holds it, else its folder name.
fn firefox_profile(browsers: &[Browser], cache2: &Path) -> (String, String) {
    let folder = cache2.parent();
    let found = browsers
        .iter()
        .filter(|b| b.kind == BrowserKind::Firefox)
        .flat_map(|b| b.profiles.iter())
        .find(|p| Some(p.data_dir.as_path()) == folder || p.cache_dir.as_deref() == folder);
    match found {
        Some(profile) => (profile.id.clone(), profile.name.clone()),
        None => {
            let name = folder
                .and_then(Path::file_name)
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            (name.clone(), name)
        }
    }
}

pub struct BrowserCaches;

impl BrowserCaches {
    pub fn new() -> Self {
        Self
    }

    fn discover_in(&self, ctx: &ScanContext<'_>, home: &Path) -> Vec<CleanItem> {
        offer(ctx, cache_folders(ctx, home), ID, LABEL)
    }
}

impl Default for BrowserCaches {
    fn default() -> Self {
        Self::new()
    }
}

impl CleanProvider for BrowserCaches {
    fn id(&self) -> &'static str {
        ID
    }
    fn label(&self) -> &'static str {
        LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Browser cache folders (never cookies, logins or history) the browser rebuilds".into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    fn desktop_trash_paths(&self) -> bool {
        true
    }
    fn item_apps(&self, path: &Path) -> Vec<String> {
        gate_apps(path)
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        Ok(match home() {
            Some(h) => self.discover_in(ctx, &h),
            None => Vec::new(),
        })
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_per_item(items, action, ID)
    }
}

/// Resolves `<profiles_root>/<profile>/cache2` for every direct child
/// profile directory.
pub fn firefox_cache_dirs(ctx: &ScanContext<'_>, profiles_root: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in list_children(profiles_root, ctx) {
        if !is_dir_safe(&p) {
            continue;
        }
        let cache2 = p.join(FIREFOX_CACHE);
        if is_dir_safe(&cache2) {
            out.push(cache2);
        }
    }
    out
}

#[cfg(test)]
mod tests;
