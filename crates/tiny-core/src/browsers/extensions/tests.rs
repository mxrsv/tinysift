//! Extension listing on made-up fixture profiles in temporary folders.
//! Ids, names and times are invented; nothing outside the fixtures is read.

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{json, Value};

use super::*;

const DAY: i64 = 86_400;
/// 2026-11-22 00:00 UTC.
const NOW: i64 = 1_795_305_600;
const WEB: &str = "aaaabbbbccccddddeeeeffffgggghhhh";
const OTHER: &str = "hhhhggggffffeeeeddddccccbbbbaaaa";

fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

/// Unix seconds as Chromium writes them: microseconds since 1601, a string.
fn chromium(unix: i64) -> String {
    ((unix + CHROMIUM_EPOCH_OFFSET_SECS) * MICROS_PER_SEC).to_string()
}

fn prefs(dir: &Path, file: &str, settings: Value) {
    let prefs = json!({ "extensions": { "settings": settings } });
    write(&dir.join(file), &prefs.to_string());
}

fn secure(dir: &Path, settings: Value) {
    prefs(dir, SECURE_PREFERENCES, settings);
}

/// A web-store entry updated `updated_days_ago` before [`NOW`].
fn store_entry(version: &str, updated_days_ago: i64) -> Value {
    json!({
        "location": 1,
        "from_webstore": true,
        "first_install_time": chromium(NOW - 800 * DAY),
        "last_update_time": chromium(NOW - updated_days_ago * DAY),
        "path": format!("{WEB}/{version}"),
        "disable_reasons": [],
    })
}

fn manifest(dir: &Path, id: &str, version: &str, body: Value) -> PathBuf {
    let folder = dir.join(EXTENSIONS_DIR).join(id).join(version);
    write(&folder.join("manifest.json"), &body.to_string());
    folder
}

fn only(list: &ProfileExtensions) -> &Extension {
    assert_eq!(list.issue, None);
    assert_eq!(list.extensions.len(), 1, "{:?}", list.extensions);
    &list.extensions[0]
}

#[test]
fn web_store_extension_has_name_version_times_and_size() {
    let profile = tempfile::tempdir().unwrap();
    let p = profile.path();
    secure(p, json!({ WEB: store_entry("1.4.0_0", 10) }));
    let folder = manifest(
        p,
        WEB,
        "1.4.0_0",
        json!({ "name": "Tab Saver", "version": "1.4.0" }),
    );
    write(&folder.join("background.js"), &"x".repeat(1000));

    let list = list_in(p, NOW);
    let ext = only(&list);
    assert_eq!(ext.id, WEB);
    assert_eq!(ext.name, "Tab Saver");
    assert_eq!(ext.version.as_deref(), Some("1.4.0"));
    assert_eq!(ext.source, ExtensionSource::Installed);
    assert!(ext.from_webstore);
    assert!(!ext.disabled);
    assert!(!ext.stale);
    assert_eq!(ext.installed_at, Some(NOW - 800 * DAY));
    assert_eq!(ext.updated_at, Some(NOW - 10 * DAY));
    assert_eq!(ext.dir.as_deref(), Some(folder.as_path()));
    let manifest_len = fs::metadata(folder.join("manifest.json")).unwrap().len();
    assert_eq!(ext.size_bytes, 1000 + manifest_len);
}

#[test]
fn serializes_as_camel_case() {
    let profile = tempfile::tempdir().unwrap();
    secure(profile.path(), json!({ WEB: store_entry("1.0_0", 1) }));
    let value = serde_json::to_value(list_in(profile.path(), NOW)).unwrap();
    let ext = &value["extensions"][0];
    assert_eq!(ext["fromWebstore"], json!(true));
    assert_eq!(ext["source"], json!("installed"));
    assert!(ext.get("sizeBytes").is_some());
}

#[test]
fn disabled_by_int_bitmask_or_reason_list() {
    let profile = tempfile::tempdir().unwrap();
    let mut by_mask = store_entry("1.0_0", 1);
    by_mask["disable_reasons"] = json!(1);
    let mut by_list = store_entry("1.0_0", 1);
    by_list["disable_reasons"] = json!([1, 8192]);
    let mut enabled = store_entry("1.0_0", 1);
    enabled["disable_reasons"] = json!(0);
    secure(
        profile.path(),
        json!({ "a": by_mask, "b": by_list, "c": enabled }),
    );

    let list = list_in(profile.path(), NOW);
    let disabled: Vec<(&str, bool)> = list
        .extensions
        .iter()
        .map(|e| (e.id.as_str(), e.disabled))
        .collect();
    assert_eq!(disabled, [("a", true), ("b", true), ("c", false)]);
}

#[test]
fn stale_only_after_a_year_without_update() {
    let profile = tempfile::tempdir().unwrap();
    let mut never_updated = store_entry("1.0_0", 0);
    never_updated
        .as_object_mut()
        .unwrap()
        .remove("last_update_time");
    secure(
        profile.path(),
        json!({
            "fresh": store_entry("1.0_0", 364),
            "old": store_entry("1.0_0", 366),
            "never": never_updated,
        }),
    );

    let list = list_in(profile.path(), NOW);
    let stale: Vec<(&str, bool)> = list
        .extensions
        .iter()
        .map(|e| (e.id.as_str(), e.stale))
        .collect();
    assert_eq!(stale, [("fresh", false), ("never", true), ("old", true)]);
    // A later `now` makes the fresh one stale too: the flag follows `now`.
    let later = list_in(profile.path(), NOW + 2 * DAY);
    assert!(later.extensions.iter().all(|e| e.stale));
}

#[test]
fn message_name_resolves_through_default_locale_without_case() {
    let profile = tempfile::tempdir().unwrap();
    let p = profile.path();
    secure(p, json!({ WEB: store_entry("2.0_0", 1) }));
    let folder = manifest(
        p,
        WEB,
        "2.0_0",
        json!({ "name": "__MSG_appName__", "default_locale": "fr", "version": "2.0" }),
    );
    write(
        &folder.join("_locales/fr/messages.json"),
        &json!({ "APPNAME": { "message": "Gardien d'onglets" } }).to_string(),
    );
    write(
        &folder.join("_locales/en/messages.json"),
        &json!({ "appName": { "message": "Tab Keeper" } }).to_string(),
    );

    assert_eq!(only(&list_in(p, NOW)).name, "Gardien d'onglets");
}

#[test]
fn message_name_falls_back_to_en_then_to_the_id() {
    let profile = tempfile::tempdir().unwrap();
    let p = profile.path();
    secure(
        p,
        json!({ WEB: store_entry("1.0_0", 1), OTHER: {
            "location": 1, "path": format!("{OTHER}/1.0_0")
        } }),
    );
    let folder = manifest(
        p,
        WEB,
        "1.0_0",
        json!({ "name": "__MSG_extName__", "default_locale": "de" }),
    );
    write(
        &folder.join("_locales/en/messages.json"),
        &json!({ "extname": { "message": "Reader" } }).to_string(),
    );
    manifest(p, OTHER, "1.0_0", json!({ "name": "__MSG_missing__" }));

    let list = list_in(p, NOW);
    let names: Vec<&str> = list.extensions.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, [OTHER, "Reader"]);
}

#[test]
fn missing_manifest_uses_the_raw_id() {
    let profile = tempfile::tempdir().unwrap();
    secure(profile.path(), json!({ WEB: store_entry("1.0_0", 1) }));

    let list = list_in(profile.path(), NOW);
    let ext = only(&list);
    assert_eq!(ext.name, WEB);
    assert_eq!(ext.version, None);
    assert_eq!(ext.dir, None);
    assert_eq!(ext.size_bytes, 0);
}

#[test]
fn highest_version_folder_when_path_is_missing_or_gone() {
    let profile = tempfile::tempdir().unwrap();
    let p = profile.path();
    let mut entry = store_entry("9.9_0", 1);
    entry.as_object_mut().unwrap().remove("path");
    secure(p, json!({ WEB: entry }));
    manifest(
        p,
        WEB,
        "1.9.0_0",
        json!({ "name": "Old", "version": "1.9.0" }),
    );
    manifest(
        p,
        WEB,
        "1.10.0_0",
        json!({ "name": "New", "version": "1.10.0" }),
    );
    fs::create_dir_all(p.join(EXTENSIONS_DIR).join(WEB).join("Temp")).unwrap();

    assert_eq!(only(&list_in(p, NOW)).version.as_deref(), Some("1.10.0"));
}

#[test]
fn unpacked_extension_is_listed_as_unpacked() {
    let profile = tempfile::tempdir().unwrap();
    let source = tempfile::tempdir().unwrap();
    write(
        &source.path().join("manifest.json"),
        &json!({ "name": "My Dev Tool", "version": "0.1" }).to_string(),
    );
    secure(
        profile.path(),
        json!({ WEB: { "location": 4, "path": source.path() } }),
    );

    let list = list_in(profile.path(), NOW);
    let ext = only(&list);
    assert_eq!(ext.source, ExtensionSource::Unpacked);
    assert_eq!(ext.name, "My Dev Tool");
    assert!(!ext.from_webstore);
    assert!(ext.size_bytes > 0);
}

#[test]
fn browser_admin_and_default_installed_extensions_are_skipped() {
    let profile = tempfile::tempdir().unwrap();
    let mut by_default = store_entry("1.0_0", 1);
    by_default["was_installed_by_default"] = json!(true);
    secure(
        profile.path(),
        json!({
            "component": { "location": 5 },
            "external": { "location": 6 },
            "policy": { "location": 9 },
            "default": by_default,
            "no-location": { "ack_external": true },
            "not-an-object": 3,
            WEB: store_entry("1.0_0", 1),
        }),
    );

    assert_eq!(only(&list_in(profile.path(), NOW)).id, WEB);
}

#[test]
fn preferences_used_when_secure_preferences_is_absent_or_has_none() {
    let profile = tempfile::tempdir().unwrap();
    let p = profile.path();
    prefs(p, PREFERENCES, json!({ WEB: store_entry("1.0_0", 1) }));
    assert_eq!(only(&list_in(p, NOW)).id, WEB);

    write(&p.join(SECURE_PREFERENCES), "{\"protection\": {}}");
    assert_eq!(only(&list_in(p, NOW)).id, WEB);
}

#[test]
fn malformed_preferences_give_an_issue_and_no_list() {
    let profile = tempfile::tempdir().unwrap();
    let p = profile.path();
    write(&p.join(SECURE_PREFERENCES), "{ not json");

    let list = list_in(p, NOW);
    assert!(list.extensions.is_empty());
    let issue = list.issue.expect("an issue");
    assert_eq!(issue.kind, DiscoveryIssueKind::Malformed);
    assert_eq!(issue.path, p.join(SECURE_PREFERENCES));
}

#[test]
fn broken_secure_preferences_are_reported_beside_a_plain_preferences() {
    let profile = tempfile::tempdir().unwrap();
    let p = profile.path();
    write(&p.join(SECURE_PREFERENCES), "[1, 2");
    write(&p.join(PREFERENCES), "{}");

    let list = list_in(p, NOW);
    assert!(list.extensions.is_empty());
    assert_eq!(list.issue.unwrap().kind, DiscoveryIssueKind::Malformed);
}

#[test]
fn missing_preferences_give_an_issue() {
    let profile = tempfile::tempdir().unwrap();
    let list = list_in(profile.path(), NOW);
    assert!(list.extensions.is_empty());
    assert_eq!(list.issue.unwrap().kind, DiscoveryIssueKind::Unreadable);
}

#[test]
fn prefs_without_extensions_give_an_empty_list_and_no_issue() {
    let profile = tempfile::tempdir().unwrap();
    write(&profile.path().join(PREFERENCES), "{}");
    assert_eq!(list_in(profile.path(), NOW), ProfileExtensions::default());
}

#[cfg(unix)]
#[test]
fn size_does_not_follow_symlinks() {
    let profile = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let p = profile.path();
    write(&outside.path().join("big.bin"), &"y".repeat(10_000));
    secure(p, json!({ WEB: store_entry("1.0_0", 1) }));
    let folder = manifest(p, WEB, "1.0_0", json!({ "name": "Linker" }));
    std::os::unix::fs::symlink(outside.path(), folder.join("link")).unwrap();

    let manifest_len = fs::metadata(folder.join("manifest.json")).unwrap().len();
    assert_eq!(only(&list_in(p, NOW)).size_bytes, manifest_len);
}

#[test]
fn non_chromium_browser_lists_nothing() {
    let profile = tempfile::tempdir().unwrap();
    secure(profile.path(), json!({ WEB: store_entry("1.0_0", 1) }));
    let safari = BrowserProfile {
        id: String::new(),
        name: "Safari".into(),
        user_name: None,
        avatar: Default::default(),
        highlight_color: None,
        data_dir: profile.path().to_path_buf(),
        cache_dir: None,
    };
    assert_eq!(
        list(BrowserKind::Safari, &safari, NOW),
        ProfileExtensions::default()
    );
    assert_eq!(list(BrowserKind::Chrome, &safari, NOW).extensions.len(), 1);
}
