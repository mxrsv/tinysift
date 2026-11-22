//! The cookie and history rules on made-up Chromium profiles. Fixture
//! databases are created in temporary folders with the real table shapes
//! (the columns read plus a few others); every domain is invented.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};

use super::*;
use crate::browsers::ProfileAvatar;

/// 2027-01-15, the fixed "now" every fixture counts back from.
pub(super) fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(1_800_000_000)
}

/// Microseconds since the Unix epoch, `days` before [`now`].
pub(super) fn unix_days_ago(days: i64) -> i64 {
    unix_micros(now()) - days * MICROS_PER_DAY
}

/// The same moment on Chromium's clock (microseconds since 1601).
fn chromium_days_ago(days: i64) -> i64 {
    unix_days_ago(days) + 11_644_473_600 * 1_000_000
}

pub(super) fn profile(dir: &Path) -> BrowserProfile {
    BrowserProfile {
        id: "Default".into(),
        name: "Person 1".into(),
        user_name: None,
        avatar: ProfileAvatar::default(),
        highlight_color: None,
        data_dir: dir.to_path_buf(),
        cache_dir: None,
    }
}

/// `urls` rows as `(url, days since the last visit)`.
pub(super) fn chromium_history(dir: &Path, rows: &[(&str, i64)]) {
    let conn = Connection::open(dir.join("History")).unwrap();
    conn.execute_batch(
        "CREATE TABLE urls(id INTEGER PRIMARY KEY AUTOINCREMENT, url LONGVARCHAR, title LONGVARCHAR,
            visit_count INTEGER DEFAULT 0 NOT NULL, typed_count INTEGER DEFAULT 0 NOT NULL,
            last_visit_time INTEGER NOT NULL, hidden INTEGER DEFAULT 0 NOT NULL);
         CREATE TABLE visits(id INTEGER PRIMARY KEY, url INTEGER NOT NULL, visit_time INTEGER NOT NULL);",
    )
    .unwrap();
    for (url, days) in rows {
        conn.execute(
            "INSERT INTO urls(url, title, visit_count, last_visit_time) VALUES (?1, 'Page', 1, ?2)",
            params![url, chromium_days_ago(*days)],
        )
        .unwrap();
    }
}

/// `cookies` rows as `(host_key, name, days since the last access)`.
pub(super) fn chromium_cookies(path: &Path, rows: &[(&str, &str, i64)]) {
    chromium_cookies_partitioned(
        path,
        &rows
            .iter()
            .map(|&(h, n, d)| (h, "", n, d))
            .collect::<Vec<_>>(),
    );
}

/// As [`chromium_cookies`], with `top_frame_site_key` as the second field.
fn chromium_cookies_partitioned(path: &Path, rows: &[(&str, &str, &str, i64)]) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(
        "CREATE TABLE cookies(creation_utc INTEGER NOT NULL, host_key TEXT NOT NULL,
            top_frame_site_key TEXT NOT NULL, name TEXT NOT NULL, value TEXT NOT NULL,
            encrypted_value BLOB NOT NULL, path TEXT NOT NULL, expires_utc INTEGER NOT NULL,
            last_access_utc INTEGER NOT NULL, source_port INTEGER NOT NULL,
            UNIQUE (host_key, top_frame_site_key, name, path, source_port));",
    )
    .unwrap();
    for (n, (host, partition, name, days)) in rows.iter().enumerate() {
        conn.execute(
            "INSERT INTO cookies VALUES (?1, ?2, ?3, ?4, '', x'00', '/', 0, ?5, ?6)",
            params![
                chromium_days_ago(400),
                host,
                partition,
                name,
                chromium_days_ago(*days),
                443 + n as i64
            ],
        )
        .unwrap();
    }
}

pub(super) fn scan_chrome(dir: &Path) -> ProfilePrivacy {
    scan_profile(BrowserKind::Chrome, &profile(dir), now())
}

fn keys(result: &ProfilePrivacy) -> Vec<(&str, &str, RemovedCookie)> {
    result
        .cookies_to_remove
        .iter()
        .map(|k| (k.host.as_str(), k.name.as_str(), k.class))
        .collect()
}

#[test]
fn a_site_visited_10_days_ago_keeps_its_cookies() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path();
    chromium_history(
        dir,
        &[
            ("https://www.example.com/home", 10),
            ("https://www.example.co.uk/", 10),
        ],
    );
    chromium_cookies(
        &dir.join("Cookies"),
        &[
            (".example.com", "sid", 200),
            ("accounts.example.co.uk", "auth", 300),
        ],
    );
    let result = scan_chrome(dir);
    assert!(result.supported, "{:?}", result.reason);
    assert_eq!((result.kept, result.old, result.trackers), (2, 0, 0));
    assert!(result.cookies_to_remove.is_empty());
    assert!(!result.history_is_empty);
}

#[test]
fn a_site_last_visited_120_days_ago_loses_its_cookies_and_history() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path();
    chromium_history(
        dir,
        &[
            ("https://example.com/", 5),
            ("https://old.example.org/a", 120),
        ],
    );
    chromium_cookies(
        &dir.join("Cookies"),
        &[(".example.org", "pref", 120), ("old.example.org", "x", 150)],
    );
    let result = scan_chrome(dir);
    assert_eq!((result.kept, result.old, result.trackers), (0, 2, 0));
    assert_eq!(result.old_sites, 1);
    assert_eq!(
        keys(&result),
        vec![
            (".example.org", "pref", RemovedCookie::Old),
            ("old.example.org", "x", RemovedCookie::Old)
        ]
    );
    assert_eq!(result.old_history_entries, 1);
    assert_eq!(result.history_to_remove, vec![2]);
    assert_eq!(result.cookie_store, Some(dir.join("Cookies")));
    assert_eq!(result.history_store, Some(dir.join("History")));
}

#[test]
fn a_never_visited_site_is_a_tracker() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path();
    chromium_history(dir, &[("https://example.com/", 3)]);
    chromium_cookies(
        &dir.join("Cookies"),
        &[
            (".ads.example.net", "uid", 100),
            (".example.net", "uid2", 95),
            (".example.com", "sid", 1),
        ],
    );
    let result = scan_chrome(dir);
    assert_eq!((result.kept, result.old, result.trackers), (1, 0, 2));
    assert_eq!(result.tracker_sites, 1);
    assert!(keys(&result).iter().all(|k| k.2 == RemovedCookie::Tracker));
    assert!(result.history_to_remove.is_empty());
}

#[test]
fn a_recent_last_access_keeps_a_cookie_of_an_unvisited_site() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path();
    chromium_history(
        dir,
        &[("https://example.com/", 3), ("https://example.org/", 200)],
    );
    chromium_cookies(
        &dir.join("Cookies"),
        &[(".example.net", "embed", 20), (".example.org", "s", 89)],
    );
    let result = scan_chrome(dir);
    assert_eq!((result.kept, result.old, result.trackers), (2, 0, 0));
}

#[test]
fn an_empty_history_marks_no_tracker() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path();
    chromium_history(dir, &[]);
    chromium_cookies(
        &dir.join("Cookies"),
        &[(".example.com", "sid", 10), (".example.net", "uid", 200)],
    );
    let result = scan_chrome(dir);
    assert!(result.history_is_empty);
    assert_eq!((result.kept, result.old, result.trackers), (1, 1, 0));
    assert_eq!(
        keys(&result),
        vec![(".example.net", "uid", RemovedCookie::Old)]
    );
}

#[test]
fn a_missing_history_file_counts_as_empty_history() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path();
    chromium_cookies(
        &dir.join("Cookies"),
        &[(".example.com", "sid", 10), (".example.net", "uid", 200)],
    );
    let result = scan_chrome(dir);
    assert!(result.supported, "{:?}", result.reason);
    assert!(result.history_is_empty);
    assert_eq!(result.history_store, None);
    assert_eq!((result.kept, result.old, result.trackers), (1, 1, 0));
}

#[test]
fn cookies_are_read_from_either_chromium_path() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path();
    chromium_history(dir, &[("https://example.com/", 3)]);
    chromium_cookies(
        &dir.join("Network").join("Cookies"),
        &[(".example.net", "uid", 100)],
    );
    let result = scan_chrome(dir);
    assert_eq!(
        result.cookie_store,
        Some(dir.join("Network").join("Cookies"))
    );
    assert_eq!(result.trackers, 1);

    // With both present, the one written last is live.
    chromium_cookies(
        &dir.join("Cookies"),
        &[(".example.org", "a", 100), (".example.org", "b", 100)],
    );
    let stale = SystemTime::now() - Duration::from_secs(86_400);
    let file = fs::File::options()
        .write(true)
        .open(dir.join("Network").join("Cookies"))
        .unwrap();
    file.set_modified(stale).unwrap();
    let result = scan_chrome(dir);
    assert_eq!(result.cookie_store, Some(dir.join("Cookies")));
    assert_eq!(result.trackers, 2);
}

#[test]
fn rows_of_one_key_are_kept_together() {
    // Two partitions of one cookie: deleting by host, name and path would
    // take both, so the recently used one keeps the other.
    let home = tempfile::tempdir().unwrap();
    let dir = home.path();
    chromium_history(dir, &[("https://example.com/", 3)]);
    chromium_cookies_partitioned(
        &dir.join("Cookies"),
        &[
            (".example.net", "https://example.com", "uid", 5),
            (".example.net", "https://example.org", "uid", 300),
            (".example.net", "", "other", 300),
        ],
    );
    let result = scan_chrome(dir);
    assert_eq!((result.kept, result.trackers), (2, 1));
    assert_eq!(
        keys(&result),
        vec![(".example.net", "other", RemovedCookie::Tracker)]
    );
}

#[test]
fn a_profile_without_databases_has_nothing_to_remove() {
    let home = tempfile::tempdir().unwrap();
    let result = scan_chrome(home.path());
    assert!(result.supported);
    assert!(result.history_is_empty);
    assert_eq!(
        (
            result.kept,
            result.old,
            result.trackers,
            result.old_history_entries
        ),
        (0, 0, 0, 0)
    );
    assert_eq!(
        (result.cookie_store.clone(), result.history_store.clone()),
        (None, None)
    );
}

#[test]
fn safari_is_not_scanned_in_this_build() {
    let home = tempfile::tempdir().unwrap();
    let result = scan_profile(BrowserKind::Safari, &profile(home.path()), now());
    assert!(!result.supported);
    let reason = result.reason.unwrap();
    assert_eq!(reason.kind, PrivacyIssueKind::NotSupported);
    assert_eq!(reason.path, None);
}

#[test]
fn the_result_serializes_in_camel_case() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path();
    chromium_history(dir, &[("https://example.com/", 3)]);
    chromium_cookies(&dir.join("Cookies"), &[(".example.net", "uid", 100)]);
    let json = serde_json::to_value(scan_chrome(dir)).unwrap();
    for field in [
        "profileId",
        "oldHistoryEntries",
        "historyIsEmpty",
        "cookiesToRemove",
        "historyToRemove",
    ] {
        assert!(json.get(field).is_some(), "{field}");
    }
    assert_eq!(json["cookiesToRemove"][0]["class"], "tracker");
    assert_eq!(
        json["cookiesToRemove"][0]["originAttributes"],
        serde_json::Value::Null
    );
}

#[test]
fn scan_browser_follows_the_profile_order() {
    let home = tempfile::tempdir().unwrap();
    let first: PathBuf = home.path().join("Default");
    let second: PathBuf = home.path().join("Profile 1");
    fs::create_dir_all(&first).unwrap();
    fs::create_dir_all(&second).unwrap();
    let browser = Browser {
        kind: BrowserKind::Brave,
        family: BrowserFamily::Chromium,
        name: "Brave".into(),
        data_root: home.path().to_path_buf(),
        container_dir: None,
        bundle_id: "com.brave.Browser".into(),
        helper_bundle_prefixes: Vec::new(),
        app_names: Vec::new(),
        profiles: vec![
            profile(&first),
            BrowserProfile {
                id: "Profile 1".into(),
                ..profile(&second)
            },
        ],
        issue: None,
    };
    let ids: Vec<String> = scan_browser(&browser, now())
        .into_iter()
        .map(|p| p.profile_id)
        .collect();
    assert_eq!(ids, ["Default", "Profile 1"]);
}
