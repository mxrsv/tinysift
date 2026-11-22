use super::*;
use crate::clean::process::test_support::MockChecker;
use crate::clean::providers::browser_offline_data::BrowserOfflineData;
use crate::clean::providers::known_category_ids;
use crate::clean::providers::user_caches::UserCaches;
use crate::runner::test_support::MockRunner;
use std::fs;
use std::sync::Arc;

const CHROME: &str = "Library/Application Support/Google/Chrome";
const CHROME_CACHES: &str = "Library/Caches/Google/Chrome";
const BRAVE: &str = "Library/Application Support/BraveSoftware/Brave-Browser";
const BRAVE_CACHES: &str = "Library/Caches/BraveSoftware/Brave-Browser";
const FIREFOX: &str = "Library/Application Support/Firefox";
const FIREFOX_PROFILE: &str = "Profiles/ab12cd34.default-release";

/// What a profile keeps that is not cache: never offered, whole or inside
/// an offered folder.
const USER_STATE: &[&str] = &[
    "Cookies",
    "Cookies-journal",
    "Login Data",
    "Preferences",
    "Secure Preferences",
    "History",
    "Bookmarks",
    "Web Data",
    "Extensions/abcdefghijklmnop/1.0/manifest.json",
    "Local Storage/leveldb/000003.log",
    "IndexedDB/https_example.test_0.indexeddb.leveldb/LOG",
    "Network/Cookies",
];

/// Lowercase names no browser-caches folder may contain.
const FORBIDDEN: &[&str] = &[
    "cookies",
    "login data",
    "preferences",
    "history",
    "local state",
    "extensions",
    "local storage",
    "indexeddb",
    "cachestorage",
    "passwords",
    "bookmarks",
    "web data",
];

fn file(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, b"x").unwrap();
}

fn chromium_profile(home: &Path, data: &str, caches: &str, profile: &str) {
    let data = home.join(data).join(profile);
    for state in USER_STATE {
        file(&data.join(state));
    }
    for cache in [
        "GPUCache/data_0",
        "Code Cache/js/index",
        "Service Worker/ScriptCache/a",
    ] {
        file(&data.join(cache));
    }
    file(&data.join("Service Worker/CacheStorage/0a1b/index"));
    file(&data.join("Service Worker/Database/CURRENT"));
    let caches = home.join(caches).join(profile);
    for cache in [
        "Cache/Cache_Data/f_000001",
        "Code Cache/wasm/index",
        "Storage/ext/a/def/x",
    ] {
        file(&caches.join(cache));
    }
}

fn local_state(home: &Path, data: &str, profiles: &[(&str, &str)]) {
    let cache: Vec<String> = profiles
        .iter()
        .map(|(id, name)| format!("\"{id}\": {{\"name\": \"{name}\"}}"))
        .collect();
    let json = format!(
        "{{\"profile\": {{\"info_cache\": {{{}}}}}}}",
        cache.join(", ")
    );
    let path = home.join(data).join("Local State");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, json).unwrap();
}

/// A made-up home with Chrome (two profiles), Brave, Firefox and Safari.
fn fixture_home(label: &str) -> PathBuf {
    let home = std::env::temp_dir().join(format!(
        "tiny-clean-br-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    local_state(
        &home,
        CHROME,
        &[("Default", "Personal"), ("Profile 1", "Work")],
    );
    chromium_profile(&home, CHROME, CHROME_CACHES, "Default");
    chromium_profile(&home, CHROME, CHROME_CACHES, "Profile 1");
    local_state(&home, BRAVE, &[("Default", "Person 1")]);
    chromium_profile(&home, BRAVE, BRAVE_CACHES, "Default");
    let ini = format!("[Profile0]\nName=default-release\nIsRelative=1\nPath={FIREFOX_PROFILE}\n");
    file(&home.join(FIREFOX).join("profiles.ini"));
    fs::write(home.join(FIREFOX).join("profiles.ini"), ini).unwrap();
    for state in ["cookies.sqlite", "places.sqlite", "prefs.js", "logins.json"] {
        file(&home.join(FIREFOX).join(FIREFOX_PROFILE).join(state));
    }
    file(
        &home
            .join("Library/Caches/Firefox")
            .join(FIREFOX_PROFILE)
            .join("cache2/entries/A"),
    );
    file(&home.join("Library/Caches/com.apple.Safari/Cache.db"));
    file(&home.join("Library/Safari/History.db"));
    file(&home.join("Library/Caches/Homebrew/downloads/x.tar.gz"));
    home
}

fn cleanup(home: &Path) {
    // The temp folder sits under `/var`, a symlink, which the safe walker
    // refuses; this is a folder the test made.
    let _ = fs::remove_dir_all(home);
}

fn relative(home: &Path, items: &[CleanItem]) -> Vec<String> {
    let mut paths: Vec<String> = items
        .iter()
        .map(|i| i.path.strip_prefix(home).unwrap().display().to_string())
        .collect();
    paths.sort();
    paths
}

/// Every file under `home` whose name says it is user state.
fn user_state_paths(home: &Path) -> Vec<PathBuf> {
    crate::clean::fs_safe::walk_no_follow(home)
        .into_iter()
        .filter(|p| {
            let lower = p
                .strip_prefix(home)
                .unwrap()
                .display()
                .to_string()
                .to_lowercase();
            FORBIDDEN.iter().any(|f| lower.contains(f))
        })
        .collect()
}

#[test]
fn browser_ids_in_known_categories() {
    assert!(known_category_ids().contains(&ID));
    assert!(known_category_ids().contains(&"browser-offline-data"));
}

#[test]
fn browser_does_not_touch_cookies_or_login() {
    // Iron law: cache-only. If a future maintainer adds a path with
    // these tokens, this test must fail loudly.
    let folders = CHROMIUM_CACHE_FOLDERS
        .iter()
        .chain(CHROMIUM_DATA_FOLDERS)
        .chain([&SAFARI_CACHE, &FIREFOX_CACHE]);
    for rel in folders {
        let lower = rel.to_lowercase();
        for forbidden in FORBIDDEN {
            assert!(
                !lower.contains(forbidden),
                "browser provider must not touch {forbidden}: {rel}"
            );
        }
    }
}

#[test]
fn every_profile_of_every_browser_is_covered_and_user_state_never_offered() {
    let home = fixture_home("cover");
    let m = MockChecker::none();
    let ctx = ScanContext::new(None, &m);
    let items = BrowserCaches::new().discover_in(&ctx, &home);
    let mut expected: Vec<String> = Vec::new();
    for (data, caches) in [
        (
            format!("{CHROME}/Default"),
            format!("{CHROME_CACHES}/Default"),
        ),
        (
            format!("{CHROME}/Profile 1"),
            format!("{CHROME_CACHES}/Profile 1"),
        ),
        (
            format!("{BRAVE}/Default"),
            format!("{BRAVE_CACHES}/Default"),
        ),
    ] {
        for folder in CHROMIUM_CACHE_FOLDERS {
            expected.push(format!("{caches}/{folder}"));
        }
        for folder in ["Code Cache", "GPUCache", "Service Worker/ScriptCache"] {
            expected.push(format!("{data}/{folder}"));
        }
    }
    expected.push(format!("Library/Caches/Firefox/{FIREFOX_PROFILE}/cache2"));
    expected.push(SAFARI_CACHE.to_string());
    expected.sort();
    assert_eq!(relative(&home, &items), expected);

    let state = user_state_paths(&home);
    assert!(state.len() > USER_STATE.len() * 3, "{state:?}");
    for item in &items {
        assert!(item.size > 0, "{}", item.path.display());
        for kept in &state {
            assert!(
                !kept.starts_with(&item.path),
                "{} offers {}",
                item.path.display(),
                kept.display()
            );
        }
    }
    let work = items
        .iter()
        .find(|i| i.path.ends_with("Profile 1/GPUCache"))
        .unwrap();
    assert_eq!(
        work.evidence,
        vec![Evidence::BrowserProfile {
            browser: "Google Chrome".into(),
            browser_kind: BrowserKind::Chrome,
            profile_id: "Profile 1".into(),
            profile_name: "Work".into(),
        }]
    );
    cleanup(&home);
}

#[test]
fn offline_site_data_is_each_chromium_profiles_cache_storage_only() {
    let home = fixture_home("offline");
    let m = MockChecker::none();
    let ctx = ScanContext::new(None, &m);
    let items = BrowserOfflineData.discover_in(&ctx, &home);
    assert_eq!(
        relative(&home, &items),
        [
            format!("{BRAVE}/Default/Service Worker/CacheStorage"),
            format!("{CHROME}/Default/Service Worker/CacheStorage"),
            format!("{CHROME}/Profile 1/Service Worker/CacheStorage"),
        ]
    );
    assert!(items.iter().all(|i| i.risk == RiskLevel::Review
        && i.category_id == "browser-offline-data"
        && i.category_label == "Offline site data"));
    cleanup(&home);
}

#[test]
fn a_running_browser_keeps_its_folders_and_not_the_others() {
    let home = fixture_home("running");
    let m = MockChecker::with_running(["Google Chrome", "Firefox"]);
    let ctx = ScanContext::new(None, &m);
    let mut items = BrowserCaches::new().discover_in(&ctx, &home);
    items.extend(BrowserOfflineData.discover_in(&ctx, &home));
    assert!(!items.is_empty());
    for item in &items {
        let path = item.path.display().to_string();
        assert!(!path.contains("/Google/Chrome/"), "Chrome leaked: {path}");
        assert!(!path.contains("/Firefox/"), "Firefox leaked: {path}");
    }
    assert!(items
        .iter()
        .any(|i| i.path.ends_with("Default/Cache") && i.path.starts_with(home.join(BRAVE_CACHES))));
    cleanup(&home);
}

#[test]
fn each_folder_is_gated_on_its_own_browser() {
    let home = Path::new("/Users/someone");
    let of = |rel: &str| browser_of(home, &home.join(rel));
    assert_eq!(
        of(&format!("{CHROME_CACHES}/Default/Cache")),
        Some(BrowserKind::Chrome)
    );
    assert_eq!(
        of(&format!("{CHROME}/Profile 1/GPUCache")),
        Some(BrowserKind::Chrome)
    );
    assert_eq!(
        of(&format!("{BRAVE_CACHES}/Default/Storage")),
        Some(BrowserKind::Brave)
    );
    assert_eq!(
        of("Library/Caches/Microsoft Edge/Default/Cache"),
        Some(BrowserKind::Edge)
    );
    assert_eq!(
        of(&format!("Library/Caches/Firefox/{FIREFOX_PROFILE}/cache2")),
        Some(BrowserKind::Firefox)
    );
    assert_eq!(of(SAFARI_CACHE), Some(BrowserKind::Safari));
    assert_eq!(
        of("Library/Caches/Google"),
        None,
        "the vendor folder is no browser's own"
    );
    let every = gate_apps(Path::new("/elsewhere"));
    assert_eq!(every.len(), BrowserKind::ALL.len());
}

/// R3: one byte is offered by one category only. `user-caches` lists the
/// children of `~/Library/Caches`; an installed browser's vendor folder is
/// left to `browser-caches`.
#[test]
fn no_path_is_offered_by_two_categories() {
    let home = fixture_home("overlap");
    let m = MockChecker::none();
    let ctx = ScanContext::new(None, &m);
    let user = UserCaches::with_runner(Arc::new(MockRunner::new()))
        .list_home_caches(&ctx, &home)
        .unwrap();
    let browser = BrowserCaches::new().discover_in(&ctx, &home);
    let offline = BrowserOfflineData.discover_in(&ctx, &home);
    assert_eq!(relative(&home, &user), ["Library/Caches/Homebrew"]);
    let categories = [&user, &browser, &offline];
    for (i, a) in categories.iter().enumerate() {
        for b in categories.iter().skip(i + 1) {
            for x in a.iter() {
                for y in b.iter() {
                    assert!(
                        !x.path.starts_with(&y.path) && !y.path.starts_with(&x.path),
                        "{} ({}) and {} ({})",
                        x.path.display(),
                        x.category_id,
                        y.path.display(),
                        y.category_id
                    );
                }
            }
        }
    }
    // Without Chrome installed, its vendor folder is an ordinary cache again.
    fs::remove_dir_all(home.join("Library/Application Support/Google")).unwrap();
    let user = UserCaches::with_runner(Arc::new(MockRunner::new()))
        .list_home_caches(&ctx, &home)
        .unwrap();
    assert_eq!(
        relative(&home, &user),
        ["Library/Caches/Google", "Library/Caches/Homebrew"]
    );
    let browser = BrowserCaches::new().discover_in(&ctx, &home);
    assert!(browser
        .iter()
        .all(|i| !i.path.starts_with(home.join("Library/Caches/Google"))));
    cleanup(&home);
}

#[test]
fn firefox_cache_dirs_resolves_profiles_glob() {
    let root = fixture_home("ff").join("Profiles");
    // Two profiles, one with cache2, one without.
    fs::create_dir_all(root.join("abcd1234.default/cache2")).unwrap();
    fs::create_dir_all(root.join("efgh5678.dev-edition")).unwrap();
    let found = firefox_cache_dirs(&ScanContext::unchecked(), &root);
    assert_eq!(found.len(), 1);
    assert!(found[0].ends_with("abcd1234.default/cache2"));
    cleanup(root.parent().unwrap());
}

/// The Browsers page measures a profile by `profile_space_folders`, so it
/// must list exactly what the two categories offer for that profile, each
/// folder in the part the page shows it under.
#[test]
fn profile_space_folders_are_what_the_categories_offer() {
    let home = fixture_home("space");
    let m = MockChecker::none();
    let ctx = ScanContext::new(None, &m);
    let mut offered = BrowserCaches::new().discover_in(&ctx, &home);
    offered.extend(BrowserOfflineData.discover_in(&ctx, &home));
    let mut offered: Vec<PathBuf> = offered.into_iter().map(|i| i.path).collect();
    offered.sort();
    let mut listed = Vec::new();
    for browser in discover(&home) {
        for profile in &browser.profiles {
            listed.extend(profile_space_folders(&home, browser.kind, profile));
        }
    }
    let mut paths: Vec<PathBuf> = listed.iter().map(|f| f.path.clone()).collect();
    paths.sort();
    assert_eq!(paths, offered);

    let part = |rel: &str| {
        listed
            .iter()
            .find(|f| f.path == home.join(rel))
            .map(|f| f.part)
    };
    assert_eq!(
        part(&format!("{CHROME_CACHES}/Default/Cache")),
        Some(SpacePart::Cache)
    );
    assert_eq!(
        part(&format!("{CHROME_CACHES}/Default/Code Cache")),
        Some(SpacePart::CodeCache)
    );
    assert_eq!(
        part(&format!("{CHROME}/Default/Code Cache")),
        Some(SpacePart::CodeCache)
    );
    assert_eq!(
        part(&format!("{CHROME_CACHES}/Default/Storage")),
        Some(SpacePart::Other)
    );
    assert_eq!(
        part(&format!("{CHROME}/Default/GPUCache")),
        Some(SpacePart::Other)
    );
    assert_eq!(
        part(&format!("{CHROME}/Default/Service Worker/CacheStorage")),
        Some(SpacePart::Offline)
    );
    assert_eq!(
        part(&format!("Library/Caches/Firefox/{FIREFOX_PROFILE}/cache2")),
        Some(SpacePart::Cache)
    );
    assert_eq!(part(SAFARI_CACHE), Some(SpacePart::Cache));

    // A running browser is still listed: the page shows what it holds.
    let running = MockChecker::with_running(["Google Chrome"]);
    let ctx = ScanContext::new(None, &running);
    assert!(BrowserCaches::new()
        .discover_in(&ctx, &home)
        .iter()
        .all(|i| !i.path.starts_with(home.join(CHROME_CACHES))));
    assert!(listed
        .iter()
        .any(|f| f.path.starts_with(home.join(CHROME_CACHES))));
    cleanup(&home);
}
