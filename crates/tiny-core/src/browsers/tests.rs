//! Discovery on made-up fixture homes in temporary folders. Names, emails
//! and pictures are invented; nothing outside the fixtures is read.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::json;

use super::*;

fn home() -> tempfile::TempDir {
    tempfile::tempdir().unwrap()
}

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn root(home: &Path, kind: BrowserKind) -> PathBuf {
    home.join(kind.data_root())
}

/// A Chromium profile folder with its `Preferences`.
fn profile_folder(home: &Path, kind: BrowserKind, folder: &str) -> PathBuf {
    let dir = root(home, kind).join(folder);
    write(&dir.join("Preferences"), "{}");
    dir
}

fn local_state(home: &Path, kind: BrowserKind, info_cache: serde_json::Value) {
    let state = json!({ "profile": { "info_cache": info_cache } });
    write(&root(home, kind).join("Local State"), &state.to_string());
}

fn only(home: &Path, kind: BrowserKind) -> Browser {
    let found: Vec<Browser> = discover(home)
        .into_iter()
        .filter(|b| b.kind == kind)
        .collect();
    assert_eq!(found.len(), 1, "{kind:?} found once");
    found.into_iter().next().unwrap()
}

fn ids(browser: &Browser) -> Vec<&str> {
    browser.profiles.iter().map(|p| p.id.as_str()).collect()
}

#[test]
fn empty_home_has_no_browsers() {
    let home = home();
    assert!(discover(home.path()).is_empty());
}

#[test]
fn chrome_lists_profiles_with_name_email_avatar_and_cache() {
    let home = home();
    let h = home.path();
    let kind = BrowserKind::Chrome;
    let default = profile_folder(h, kind, "Default");
    profile_folder(h, kind, "Profile 2");
    write(&default.join("Google Profile Picture.png"), "png");
    let cache = h.join("Library/Caches/Google/Chrome/Default");
    fs::create_dir_all(&cache).unwrap();
    local_state(
        h,
        kind,
        json!({
            "Default": {
                "name": "Work",
                "user_name": "someone@example.com",
                "avatar_icon": "chrome://theme/IDR_PROFILE_AVATAR_26",
                "use_gaia_picture": true,
                "gaia_picture_file_name": "Google Profile Picture.png",
                "profile_highlight_color": -16033840
            },
            "Profile 2": { "name": "Home", "user_name": "" }
        }),
    );

    let chrome = only(h, kind);
    assert_eq!(chrome.family, BrowserFamily::Chromium);
    assert_eq!(chrome.name, "Google Chrome");
    assert_eq!(chrome.bundle_id, "com.google.Chrome");
    assert_eq!(chrome.app_names, vec!["Google Chrome"]);
    assert!(chrome.issue.is_none());
    assert_eq!(ids(&chrome), vec!["Default", "Profile 2"]);

    let work = &chrome.profiles[0];
    assert_eq!(work.name, "Work");
    assert_eq!(work.user_name.as_deref(), Some("someone@example.com"));
    assert_eq!(work.data_dir, default);
    assert_eq!(work.cache_dir.as_deref(), Some(cache.as_path()));
    assert_eq!(work.highlight_color, Some(-16033840_i32 as u32));
    assert_eq!(work.avatar.icon_index, Some(26));
    assert!(work.avatar.uses_account_picture);
    assert_eq!(
        work.avatar.account_picture,
        Some(default.join("Google Profile Picture.png"))
    );

    let home_profile = &chrome.profiles[1];
    assert_eq!(
        home_profile.user_name, None,
        "empty user_name is signed out"
    );
    assert_eq!(home_profile.avatar, ProfileAvatar::default());
    assert_eq!(home_profile.cache_dir, None);
}

#[test]
fn account_picture_missing_on_disk_is_none() {
    let home = home();
    let h = home.path();
    let kind = BrowserKind::Chrome;
    profile_folder(h, kind, "Default");
    local_state(
        h,
        kind,
        json!({ "Default": {
            "name": "Person 1",
            "use_gaia_picture": true,
            "gaia_picture_file_name": "Google Profile Picture.png"
        }}),
    );
    let avatar = &only(h, kind).profiles[0].avatar;
    assert!(avatar.uses_account_picture);
    assert_eq!(avatar.account_picture, None);
}

#[test]
fn picture_name_with_a_path_is_refused() {
    let home = home();
    let h = home.path();
    let kind = BrowserKind::Chrome;
    profile_folder(h, kind, "Default");
    write(&root(h, kind).join("outside.png"), "png");
    local_state(
        h,
        kind,
        json!({ "Default": { "gaia_picture_file_name": "../outside.png" } }),
    );
    assert_eq!(only(h, kind).profiles[0].avatar.account_picture, None);
}

#[test]
fn listed_profile_missing_on_disk_is_skipped_and_unlisted_folder_ignored() {
    let home = home();
    let h = home.path();
    let kind = BrowserKind::Chrome;
    profile_folder(h, kind, "Default");
    profile_folder(h, kind, "Profile 9");
    local_state(
        h,
        kind,
        json!({
            "Default": { "name": "Person 1" },
            "Profile 3": { "name": "Gone" },
            "../escape": { "name": "Escape" }
        }),
    );
    assert_eq!(ids(&only(h, kind)), vec!["Default"]);
}

#[test]
fn profiles_follow_profiles_order() {
    let home = home();
    let h = home.path();
    let kind = BrowserKind::Chrome;
    for folder in ["Default", "Profile 1", "Profile 2"] {
        profile_folder(h, kind, folder);
    }
    let state = json!({ "profile": {
        "profiles_order": ["Profile 2", "Default"],
        "info_cache": { "Default": {}, "Profile 1": {}, "Profile 2": {} }
    }});
    write(&root(h, kind).join("Local State"), &state.to_string());
    let chrome = only(h, kind);
    assert_eq!(ids(&chrome), vec!["Profile 2", "Default", "Profile 1"]);
    assert_eq!(chrome.profiles[2].name, "Profile 1", "no name falls back");
}

#[test]
fn missing_local_state_with_a_profile_folder_reports_an_issue() {
    let home = home();
    let h = home.path();
    let kind = BrowserKind::Brave;
    profile_folder(h, kind, "Default");
    let brave = only(h, kind);
    assert!(brave.profiles.is_empty());
    let issue = brave.issue.expect("issue");
    assert_eq!(issue.kind, DiscoveryIssueKind::Unreadable);
    assert_eq!(issue.path, root(h, kind).join("Local State"));
}

#[test]
fn malformed_local_state_reports_an_issue() {
    for contents in [
        "{ not json",
        "[1, 2]",
        r#"{"profile": 3}"#,
        r#"{"profile": {"info_cache": []}}"#,
    ] {
        let home = home();
        let h = home.path();
        let kind = BrowserKind::Edge;
        profile_folder(h, kind, "Default");
        write(&root(h, kind).join("Local State"), contents);
        let edge = only(h, kind);
        assert!(edge.profiles.is_empty(), "{contents}");
        assert_eq!(
            edge.issue.map(|i| i.kind),
            Some(DiscoveryIssueKind::Malformed),
            "{contents}"
        );
    }
}

#[test]
fn unreadable_local_state_is_permission_denied() {
    use std::os::unix::fs::PermissionsExt;
    let home = home();
    let h = home.path();
    let kind = BrowserKind::Chrome;
    profile_folder(h, kind, "Default");
    local_state(h, kind, json!({ "Default": {} }));
    let path = root(h, kind).join("Local State");
    fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
    let readable = fs::read(&path).is_ok();
    let chrome = only(h, kind);
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    if readable {
        return; // Running as root: the mode does not deny the read.
    }
    assert!(chrome.profiles.is_empty());
    assert_eq!(
        chrome.issue.map(|i| i.kind),
        Some(DiscoveryIssueKind::PermissionDenied)
    );
}

#[test]
fn each_chromium_browser_is_found_at_its_root() {
    let cases = [
        (BrowserKind::Chrome, "com.google.Chrome", "Google Chrome"),
        (BrowserKind::Arc, "company.thebrowser.Browser", "Arc"),
        (BrowserKind::Brave, "com.brave.Browser", "Brave Browser"),
        (BrowserKind::Edge, "com.microsoft.edgemac", "Microsoft Edge"),
        (BrowserKind::Vivaldi, "com.vivaldi.Vivaldi", "Vivaldi"),
        (BrowserKind::Opera, "com.operasoftware.Opera", "Opera"),
    ];
    for (kind, bundle, app) in cases {
        let home = home();
        let h = home.path();
        let dir = profile_folder(h, kind, "Default");
        local_state(h, kind, json!({ "Default": { "name": "Person 1" } }));
        let browser = only(h, kind);
        assert_eq!(browser.bundle_id, bundle);
        assert_eq!(browser.app_names, vec![app]);
        assert_eq!(browser.profiles.len(), 1, "{kind:?}");
        assert_eq!(browser.profiles[0].data_dir, dir, "{kind:?}");
    }
}

#[test]
fn arc_cache_keeps_the_user_data_path() {
    let home = home();
    let h = home.path();
    let kind = BrowserKind::Arc;
    profile_folder(h, kind, "Profile 1");
    let cache = h.join("Library/Caches/Arc/User Data/Profile 1");
    fs::create_dir_all(&cache).unwrap();
    local_state(h, kind, json!({ "Profile 1": {} }));
    assert_eq!(only(h, kind).profiles[0].cache_dir, Some(cache));
}

#[test]
fn vivaldi_and_opera_with_only_native_messaging_hosts_are_not_reported() {
    let home = home();
    let h = home.path();
    for kind in [BrowserKind::Vivaldi, BrowserKind::Opera] {
        write(
            &root(h, kind).join("NativeMessagingHosts/com.example.host.json"),
            "{}",
        );
    }
    assert!(discover(h).is_empty());
}

#[test]
fn opera_root_level_profile_without_info_cache() {
    let home = home();
    let h = home.path();
    let kind = BrowserKind::Opera;
    let opera_root = root(h, kind);
    write(&opera_root.join("Preferences"), "{}");
    write(&opera_root.join("Local State"), "{}");
    let cache = h.join("Library/Caches/com.operasoftware.Opera");
    fs::create_dir_all(&cache).unwrap();
    let opera = only(h, kind);
    assert!(opera.issue.is_none());
    assert_eq!(opera.profiles.len(), 1);
    let profile = &opera.profiles[0];
    assert_eq!(profile.id, "");
    assert_eq!(profile.name, "Opera");
    assert_eq!(profile.data_dir, opera_root);
    assert_eq!(profile.cache_dir, Some(cache));
}

#[test]
fn opera_default_entry_resolves_to_the_root() {
    let home = home();
    let h = home.path();
    let kind = BrowserKind::Opera;
    write(&root(h, kind).join("Preferences"), "{}");
    local_state(
        h,
        kind,
        json!({ "Default": { "name": "Me", "user_name": "someone@example.org" } }),
    );
    let opera = only(h, kind);
    assert_eq!(ids(&opera), vec!["Default"], "root listed once");
    assert_eq!(opera.profiles[0].data_dir, root(h, kind));
    assert_eq!(
        opera.profiles[0].user_name.as_deref(),
        Some("someone@example.org")
    );
}

#[test]
fn root_profile_is_opera_only() {
    let home = home();
    let h = home.path();
    let kind = BrowserKind::Chrome;
    write(&root(h, kind).join("Preferences"), "{}");
    local_state(h, kind, json!({ "Default": {} }));
    assert!(only(h, kind).profiles.is_empty());
}

#[test]
fn safari_is_one_profile_when_its_folder_or_container_exists() {
    let home = home();
    let h = home.path();
    fs::create_dir_all(h.join("Library/Containers/com.apple.Safari")).unwrap();
    let safari = only(h, BrowserKind::Safari);
    assert_eq!(safari.family, BrowserFamily::WebKit);
    assert_eq!(safari.bundle_id, "com.apple.Safari");
    assert!(safari.helper_bundle_prefixes.is_empty());
    assert_eq!(
        safari.container_dir,
        Some(h.join("Library/Containers/com.apple.Safari"))
    );
    assert_eq!(safari.profiles.len(), 1);
    assert_eq!(safari.profiles[0].name, "Safari");
    assert_eq!(safari.profiles[0].user_name, None);

    fs::create_dir_all(h.join("Library/Safari")).unwrap();
    fs::create_dir_all(h.join("Library/Caches/com.apple.Safari")).unwrap();
    let safari = only(h, BrowserKind::Safari);
    assert_eq!(safari.profiles[0].data_dir, h.join("Library/Safari"));
    assert_eq!(
        safari.profiles[0].cache_dir,
        Some(h.join("Library/Caches/com.apple.Safari"))
    );
}

#[test]
fn firefox_lists_profiles_from_profiles_ini() {
    let home = home();
    let h = home.path();
    let ff = root(h, BrowserKind::Firefox);
    let absolute = h.join("Elsewhere/abc.custom");
    fs::create_dir_all(ff.join("Profiles/k1.default-release")).unwrap();
    fs::create_dir_all(ff.join("Profiles/k2.unnamed")).unwrap();
    fs::create_dir_all(&absolute).unwrap();
    let cache = h.join("Library/Caches/Firefox/Profiles/k1.default-release");
    fs::create_dir_all(&cache).unwrap();
    let ini = format!(
        "; comment\n[Install4F96D1932A9F858E]\nDefault=Profiles/k1.default-release\n\n\
         [Profile0]\nName=default-release\nIsRelative=1\nPath=Profiles/k1.default-release\n\n\
         [Profile1]\nIsRelative=1\nPath=Profiles/k2.unnamed\n\n\
         [Profile2]\nName=gone\nIsRelative=1\nPath=Profiles/k3.gone\n\n\
         [Profile3]\nName=escape\nIsRelative=1\nPath=../escape\n\n\
         [Profile4]\nName=custom\nIsRelative=0\nPath={}\n\n\
         [General]\nStartWithLastProfile=1\nVersion=2\n",
        absolute.display()
    );
    write(&ff.join("profiles.ini"), &ini);

    let firefox = only(h, BrowserKind::Firefox);
    assert_eq!(firefox.family, BrowserFamily::Gecko);
    assert!(firefox.issue.is_none());
    let names: Vec<&str> = firefox.profiles.iter().map(|p| p.name.as_str()).collect();
    assert_eq!(names, vec!["default-release", "k2.unnamed", "custom"]);
    assert_eq!(firefox.profiles[0].id, "Profiles/k1.default-release");
    assert_eq!(firefox.profiles[0].cache_dir, Some(cache));
    assert_eq!(firefox.profiles[2].data_dir, absolute);
}

#[test]
fn firefox_without_profiles_ini_is_not_reported_and_bad_ini_is_an_issue() {
    let home = home();
    let h = home.path();
    let ff = root(h, BrowserKind::Firefox);
    fs::create_dir_all(ff.join("Profiles/k1.default")).unwrap();
    assert!(discover(h).is_empty());

    write(
        &ff.join("profiles.ini"),
        "[Profile0]\nthis line is not ini\n",
    );
    let firefox = only(h, BrowserKind::Firefox);
    assert!(firefox.profiles.is_empty());
    assert_eq!(
        firefox.issue.map(|i| i.kind),
        Some(DiscoveryIssueKind::Malformed)
    );
}

#[test]
fn browsers_come_in_page_order() {
    let home = home();
    let h = home.path();
    fs::create_dir_all(h.join("Library/Safari")).unwrap();
    write(
        &root(h, BrowserKind::Firefox).join("profiles.ini"),
        "[General]\nVersion=2\n",
    );
    for kind in [BrowserKind::Brave, BrowserKind::Chrome] {
        profile_folder(h, kind, "Default");
        local_state(h, kind, json!({ "Default": {} }));
    }
    let kinds: Vec<BrowserKind> = discover(h).iter().map(|b| b.kind).collect();
    assert_eq!(
        kinds,
        vec![
            BrowserKind::Chrome,
            BrowserKind::Brave,
            BrowserKind::Safari,
            BrowserKind::Firefox
        ]
    );
}

#[test]
fn bundle_ids_cover_helpers_without_case() {
    let chrome = BrowserKind::Chrome;
    assert!(chrome.owns_bundle_id("com.google.Chrome"));
    assert!(chrome.owns_bundle_id("com.google.chrome.helper.renderer"));
    assert!(chrome.owns_bundle_id("com.google.Chrome.framework.AlertNotificationService"));
    assert!(!chrome.owns_bundle_id("com.google.Chrome2"));
    assert!(!chrome.owns_bundle_id("com.brave.Browser.helper"));
    assert!(BrowserKind::Safari.owns_bundle_id("com.apple.Safari"));
    assert!(!BrowserKind::Safari.owns_bundle_id("com.apple.Safari.SafeBrowsing.Service"));
}

/// Firefox's other editions share its profiles, so they hold it too; its
/// content processes run inside `Firefox.app`, which the running check
/// reads as Firefox itself.
#[test]
fn every_firefox_edition_holds_firefox() {
    let firefox = BrowserKind::Firefox;
    for id in [
        "org.mozilla.firefox",
        "org.mozilla.firefoxdeveloperedition",
        "org.mozilla.nightly",
        "org.mozilla.Nightly",
    ] {
        assert!(firefox.owns_bundle_id(id), "{id}");
    }
    assert!(!firefox.owns_bundle_id("org.mozilla.thunderbird"));
    assert!(!firefox.owns_bundle_id("org.mozilla.firefox2"));
    assert!(!BrowserKind::Chrome.owns_bundle_id("org.mozilla.nightly"));
    // `pgrep -x` tells case: the process is `firefox`.
    for name in [
        "firefox",
        "Firefox",
        "Firefox Developer Edition",
        "Firefox Nightly",
    ] {
        assert!(firefox.app_names().contains(&name), "{name}");
    }
}

#[test]
fn serializes_as_camel_case() {
    let home = home();
    let h = home.path();
    profile_folder(h, BrowserKind::Chrome, "Default");
    local_state(
        h,
        BrowserKind::Chrome,
        json!({ "Default": { "user_name": "someone@example.com" } }),
    );
    let value = serde_json::to_value(discover(h)).unwrap();
    assert_eq!(value[0]["kind"], "chrome");
    assert_eq!(value[0]["profiles"][0]["userName"], "someone@example.com");
    let back: Vec<Browser> = serde_json::from_value(value).unwrap();
    assert_eq!(back, discover(h));
}
