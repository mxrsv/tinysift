//! The Privacy write on made-up profiles: only consented keys move, a
//! running browser and an unknown schema refuse, the backup is in the Trash
//! before anything is written, history goes with its leftovers.

use std::fs;

use rusqlite::{params, Connection};

use super::sha256::digest_path;
use super::tests_write_fixture::*;
use super::*;

fn key(scan: &ProfilePrivacy, host: &str) -> CookieKey {
    scan.cookies_to_remove
        .iter()
        .find(|k| k.host == host)
        .unwrap_or_else(|| panic!("{host} not listed"))
        .clone()
}

fn cookie_hosts(home: &Home) -> Vec<String> {
    let conn = Connection::open(home.dir().join("Cookies")).unwrap();
    let mut statement = conn
        .prepare("SELECT host_key FROM cookies ORDER BY host_key")
        .unwrap();
    statement
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect()
}

#[test]
fn only_listed_keys_move_and_a_missing_key_is_reported() {
    let home = Home::new();
    chrome_profile(&home);
    let scan = scan_of(&home, BrowserKind::Chrome);
    assert_eq!(scan.cookies_to_remove.len(), 3, "{scan:?}");
    let mut ghost = key(&scan, "tracker.example.biz");
    ghost.host = "ghost.example.io".into();
    let consent = PrivacyConsent {
        cookies: vec![
            key(&scan, "tracker.example.biz"),
            key(&scan, ".old.example.org"),
            ghost.clone(),
        ],
        history: Vec::new(),
    };
    let history_before = fs::read(home.dir().join("History")).unwrap();
    let report = write(&home, BrowserKind::Chrome, &consent, &quit(), &home.trash()).unwrap();
    assert_eq!(report.removed_cookies, 2);
    assert_eq!(report.removed_history_entries, 0);
    assert_eq!(
        report.skipped_cookies,
        vec![SkippedCookie {
            key: ghost,
            reason: SkipReason::NotFound
        }]
    );
    assert_eq!(
        cookie_hosts(&home),
        [".ads.example.net", ".keep.example.com"]
    );
    // History was not part of the edit, so neither edited nor backed up.
    assert_eq!(
        fs::read(home.dir().join("History")).unwrap(),
        history_before
    );
    assert_eq!(report.backed_up, [home.dir().join("Cookies")]);
    assert!(report.undo_id.is_some());
}

#[test]
fn a_site_visited_since_the_scan_keeps_its_cookies() {
    let home = Home::new();
    chrome_profile(&home);
    let scan = scan_of(&home, BrowserKind::Chrome);
    // The user opens the old site again before cleaning.
    Connection::open(home.dir().join("History"))
        .unwrap()
        .execute(
            "INSERT INTO urls(url, last_visit_time) VALUES ('https://old.example.org/new', ?1)",
            params![tests_chromium_now()],
        )
        .unwrap();
    let report = write(
        &home,
        BrowserKind::Chrome,
        &consent_all(&scan),
        &quit(),
        &home.trash(),
    )
    .unwrap();
    assert_eq!(report.removed_cookies, 2);
    assert_eq!(
        report.skipped_cookies,
        vec![SkippedCookie {
            key: key(&scan, ".old.example.org"),
            reason: SkipReason::NowKept
        }]
    );
    assert_eq!(
        cookie_hosts(&home),
        [".keep.example.com", ".old.example.org"]
    );
}

/// Chromium's clock, one day before the fixtures' "now".
fn tests_chromium_now() -> i64 {
    super::tests::unix_days_ago(1) + 11_644_473_600 * 1_000_000
}

#[test]
fn the_backup_is_in_the_trash_before_the_write_and_holds_the_originals() {
    let home = Home::new();
    chrome_profile(&home);
    let before = home.listing();
    let original_cookies = digest_path(&home.dir().join("Cookies")).unwrap();
    let trash = home.trash();
    let scan = scan_of(&home, BrowserKind::Chrome);
    let report = write(
        &home,
        BrowserKind::Chrome,
        &consent_all(&scan),
        &quit(),
        &trash,
    )
    .unwrap();
    let seen = trash.seen.lock().unwrap().clone();
    // First move: the backup folder, while Cookies was still the original.
    let (folder, cookies_then) = &seen[0];
    assert_eq!(Some(folder), report.backup.as_ref());
    assert_eq!(cookies_then.as_deref(), Some(original_cookies.as_str()));
    assert!(folder.starts_with(home.trash_dir()));
    let backup = listing(folder);
    let names = [
        "Cookies",
        "Favicons",
        "History",
        "Shortcuts",
        "Top Sites",
        "Visited Links",
    ];
    assert_eq!(backup.keys().map(String::as_str).collect::<Vec<_>>(), names);
    for (name, bytes) in &backup {
        assert_eq!(&before[name], bytes, "{name}");
    }
    assert_ne!(home.listing(), before);
    // Nothing is left in staging.
    let staging = home.store().dir().join("staging");
    assert_eq!(fs::read_dir(&staging).map_or(0, |d| d.count()), 0);
}

#[test]
fn a_failing_backup_writes_nothing() {
    let home = Home::new();
    chrome_profile(&home);
    let before = home.listing();
    let mut trash = home.trash();
    trash.fail = true;
    let scan = scan_of(&home, BrowserKind::Chrome);
    let error = write(
        &home,
        BrowserKind::Chrome,
        &consent_all(&scan),
        &quit(),
        &trash,
    )
    .unwrap_err();
    assert!(matches!(error, PrivacyWriteError::Backup(_)), "{error:?}");
    assert_eq!(home.listing(), before);
    let staging = home.store().dir().join("staging");
    assert_eq!(fs::read_dir(&staging).map_or(0, |d| d.count()), 0);
}

#[test]
fn history_goes_with_its_visits_and_leftovers_only_for_those_urls() {
    let home = Home::new();
    chrome_profile(&home);
    let scan = scan_of(&home, BrowserKind::Chrome);
    assert_eq!(scan.history_to_remove, [2, 3]);
    let consent = PrivacyConsent {
        cookies: Vec::new(),
        history: vec![2, 3, 99],
    };
    let trash = home.trash();
    let report = write(&home, BrowserKind::Chrome, &consent, &quit(), &trash).unwrap();
    assert_eq!(report.removed_history_entries, 2);
    assert_eq!(report.removed_leftovers, 6);
    assert_eq!(
        report.skipped_history,
        [SkippedHistory {
            id: 99,
            reason: SkipReason::NotFound
        }]
    );
    let dir = home.dir();
    let only_keep = |file: &str, sql: &str| {
        assert_eq!(count(&dir.join(file), sql), 1, "{file}: {sql}");
    };
    only_keep("History", "SELECT count(*) FROM urls");
    only_keep("History", "SELECT count(*) FROM urls WHERE id = 1");
    assert_eq!(
        count(&dir.join("History"), "SELECT count(*) FROM visits"),
        2
    );
    assert_eq!(
        count(
            &dir.join("History"),
            "SELECT count(*) FROM visits WHERE url = 1"
        ),
        2
    );
    for (file, sql) in [
        ("Favicons", "SELECT count(*) FROM icon_mapping"),
        ("Top Sites", "SELECT count(*) FROM top_sites"),
        ("Shortcuts", "SELECT count(*) FROM omni_box_shortcuts"),
    ] {
        only_keep(file, sql);
    }
    only_keep(
        "Favicons",
        &format!("SELECT count(*) FROM icon_mapping WHERE page_url = '{KEEP_URL}'"),
    );
    only_keep(
        "Top Sites",
        &format!("SELECT count(*) FROM top_sites WHERE url = '{KEEP_URL}'"),
    );
    only_keep(
        "Shortcuts",
        &format!("SELECT count(*) FROM omni_box_shortcuts WHERE url = '{KEEP_URL}'"),
    );
    // Visited Links went to the Trash whole; cookies were not touched.
    assert!(!dir.join("Visited Links").exists());
    assert!(home.trashed().iter().any(|n| n == "Visited Links"));
    assert_eq!(cookie_hosts(&home).len(), 4);
}

#[test]
fn firefox_bookmarks_survive_and_cookies_match_their_container() {
    let home = Home::new();
    firefox_profile(&home, false);
    let scan = scan_of(&home, BrowserKind::Firefox);
    assert_eq!(scan.history_to_remove, [2], "{scan:?}");
    let container = scan
        .cookies_to_remove
        .iter()
        .find(|k| k.origin_attributes.as_deref() == Some("^userContextId=2"))
        .unwrap()
        .clone();
    let consent = PrivacyConsent {
        cookies: vec![container],
        // 3 is bookmarked: never listed, so never taken.
        history: vec![2, 3],
    };
    let report = write(
        &home,
        BrowserKind::Firefox,
        &consent,
        &quit(),
        &home.trash(),
    )
    .unwrap();
    assert_eq!(
        (report.removed_cookies, report.removed_history_entries),
        (1, 1)
    );
    assert_eq!(
        report.skipped_history,
        [SkippedHistory {
            id: 3,
            reason: SkipReason::NowKept
        }]
    );
    let places = home.dir().join("places.sqlite");
    assert_eq!(
        count(&places, "SELECT count(*) FROM moz_places WHERE id = 2"),
        0
    );
    assert_eq!(
        count(
            &places,
            "SELECT count(*) FROM moz_historyvisits WHERE place_id = 2"
        ),
        0
    );
    assert_eq!(
        count(
            &places,
            "SELECT count(*) FROM moz_places WHERE id IN (1, 3)"
        ),
        2
    );
    assert_eq!(count(&places, "SELECT count(*) FROM moz_historyvisits"), 4);
    let cookies = home.dir().join("cookies.sqlite");
    assert_eq!(
        count(
            &cookies,
            "SELECT count(*) FROM moz_cookies WHERE host = '.example.org'"
        ),
        1
    );
    assert_eq!(
        count(
            &cookies,
            "SELECT count(*) FROM moz_cookies WHERE originAttributes = '^userContextId=2'"
        ),
        0
    );
}

#[test]
fn the_report_serializes_in_camel_case() {
    let report = PrivacyWriteReport {
        undo_id: Some("x".into()),
        ..PrivacyWriteReport::default()
    };
    let json = serde_json::to_value(&report).unwrap();
    assert!(json.get("removedHistoryEntries").is_some());
    assert!(json.get("skippedCookies").is_some());
    assert_eq!(json["undoId"], "x");
}
