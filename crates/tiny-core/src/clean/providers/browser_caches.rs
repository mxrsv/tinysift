use std::path::{Path, PathBuf};

use crate::error::Result;

use super::{execute_per_item, root_as_item, CleanProvider};
use crate::clean::fs_safe::{is_dir_safe, list_children};
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};

const ID: &str = "browser-caches";
const LABEL: &str = "Browser caches";

/// Cache-only paths. NEVER include Cookies, Login Data, Preferences,
/// History — those carry user state that's not "cache". Test below pins
/// this invariant.
const BROWSER_CACHE_PATHS: &[(&str, &str)] = &[
    ("Library/Caches/com.apple.Safari", "Safari"),
    (
        "Library/Application Support/Google/Chrome/Default/Cache",
        "Google Chrome",
    ),
    (
        "Library/Application Support/Arc/User Data/Default/Cache",
        "Arc",
    ),
];

/// Firefox cache lives at `Profiles/<random>/cache2`. Resolved at
/// discover via read_dir.
const FIREFOX_PROFILES_ROOT: &str = "Library/Application Support/Firefox/Profiles";
const FIREFOX_APP: &str = "Firefox";

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub struct BrowserCaches;

impl BrowserCaches {
    pub fn new() -> Self {
        Self
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
        let owner = home().and_then(|h| {
            if path.starts_with(h.join(FIREFOX_PROFILES_ROOT)) {
                return Some(FIREFOX_APP);
            }
            BROWSER_CACHE_PATHS
                .iter()
                .find(|(rel, _)| path == h.join(rel))
                .map(|(_, app)| *app)
        });
        // An unrecognised path is gated on every browser rather than none.
        owner.map(|app| vec![app.to_string()]).unwrap_or_else(|| {
            BROWSER_CACHE_PATHS
                .iter()
                .map(|(_, app)| *app)
                .chain([FIREFOX_APP])
                .map(String::from)
                .collect()
        })
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let h = match home() {
            Some(h) => h,
            None => return Ok(Vec::new()),
        };
        let mut items = Vec::new();
        for (rel, app) in BROWSER_CACHE_PATHS {
            let path = h.join(rel);
            if !is_dir_safe(&path) {
                continue;
            }
            if ctx.known_running(app) {
                continue;
            }
            items.extend(root_as_item(ctx, &path, ID, LABEL, RiskLevel::Review));
        }
        // Firefox glob.
        let firefox_running = ctx.known_running(FIREFOX_APP);
        for cache2 in firefox_cache_dirs(ctx, &h.join(FIREFOX_PROFILES_ROOT)) {
            if firefox_running {
                continue;
            }
            items.extend(root_as_item(ctx, &cache2, ID, LABEL, RiskLevel::Review));
        }
        Ok(items)
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_per_item(items, action, ID)
    }
}

/// Resolves `<profiles_root>/<profile>/cache2` for every direct child
/// profile directory.
pub fn firefox_cache_dirs(ctx: &ScanContext<'_>, profiles_root: &std::path::Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in list_children(profiles_root, ctx) {
        if !is_dir_safe(&p) {
            continue;
        }
        let cache2 = p.join("cache2");
        if is_dir_safe(&cache2) {
            out.push(cache2);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::process::test_support::MockChecker;
    use crate::clean::providers::known_category_ids;
    use std::fs;

    fn tempdir(label: &str) -> PathBuf {
        let mut base = std::env::temp_dir();
        base.push(format!(
            "tiny-clean-br-{}-{}",
            label,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn browser_id_in_known_categories() {
        assert!(known_category_ids().contains(&ID));
    }

    #[test]
    fn browser_does_not_touch_cookies_or_login() {
        // Iron law: cache-only. If a future maintainer adds a path with
        // these tokens, this test must fail loudly.
        for (rel, _) in BROWSER_CACHE_PATHS {
            let lower = rel.to_lowercase();
            for forbidden in [
                "cookies",
                "login data",
                "history",
                "preferences",
                "passwords",
                "bookmarks",
            ] {
                assert!(
                    !lower.contains(forbidden),
                    "browser provider must not touch {}: {}",
                    forbidden,
                    rel
                );
            }
        }
    }

    #[test]
    fn browser_paths_skip_running_apps() {
        let mock = MockChecker::with_running(["Safari", "Google Chrome", "Arc", "Firefox"]);
        let ctx = ScanContext::new(None, &mock);
        let items = BrowserCaches::new().discover(&ctx).unwrap();
        for item in &items {
            let s = item.path.to_string_lossy();
            assert!(!s.contains("com.apple.Safari"), "Safari leaked: {}", s);
            assert!(!s.contains("/Google/Chrome/"), "Chrome leaked: {}", s);
            assert!(!s.contains("/Arc/"), "Arc leaked: {}", s);
            assert!(!s.contains("Firefox/Profiles"), "Firefox leaked: {}", s);
        }
    }

    #[test]
    fn firefox_cache_dirs_resolves_profiles_glob() {
        let root = tempdir("ff");
        // Two profiles, one with cache2, one without.
        fs::create_dir_all(root.join("abcd1234.default/cache2")).unwrap();
        fs::create_dir_all(root.join("efgh5678.dev-edition")).unwrap();
        let found = firefox_cache_dirs(&ScanContext::unchecked(), &root);
        assert_eq!(found.len(), 1);
        assert!(found[0].ends_with("abcd1234.default/cache2"));
        let _ = crate::clean::fs_safe::remove_recursive_safe(&root);
    }
}
