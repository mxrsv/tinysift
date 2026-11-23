//! Firefox, unknown schemas and the read-only promise: the originals stay
//! byte-identical and nothing appears beside them. Made-up fixtures only.

use std::collections::BTreeMap;
use std::fs;
use std::path::Path;

use rusqlite::{params, Connection};

use super::tests::{chromium_cookies, chromium_history, now, profile, scan_chrome, unix_days_ago};
use super::*;

/// `moz_places` rows as `(id, url, days since the last visit, foreign_count)`.
fn firefox_places(conn: &Connection, rows: &[(i64, &str, Option<i64>, i64)]) {
    conn.execute_batch(
        "CREATE TABLE moz_places(id INTEGER PRIMARY KEY, url LONGVARCHAR, title LONGVARCHAR,
            rev_host LONGVARCHAR, visit_count INTEGER DEFAULT 0, last_visit_date INTEGER,
            guid TEXT, foreign_count INTEGER DEFAULT 0 NOT NULL);",
    )
    .unwrap();
    for (id, url, days, foreign) in rows {
        conn.execute(
            "INSERT INTO moz_places(id, url, last_visit_date, foreign_count) VALUES (?1, ?2, ?3, ?4)",
            params![id, url, days.map(unix_days_ago), foreign],
        )
        .unwrap();
    }
}

/// `moz_cookies` rows as `(host, name, origin attributes, days since the last access)`.
fn firefox_cookies(conn: &Connection, rows: &[(&str, &str, &str, i64)]) {
    conn.execute_batch(
        "CREATE TABLE moz_cookies(id INTEGER PRIMARY KEY, originAttributes TEXT NOT NULL DEFAULT '',
            name TEXT, value TEXT, host TEXT, path TEXT, expiry INTEGER, lastAccessed INTEGER,
            creationTime INTEGER, isSecure INTEGER,
            CONSTRAINT moz_uniqueid UNIQUE (name, host, path, originAttributes));",
    )
    .unwrap();
    for (host, name, origin, days) in rows {
        conn.execute(
            "INSERT INTO moz_cookies(originAttributes, name, value, host, path, lastAccessed)
             VALUES (?1, ?2, 'v', ?3, '/', ?4)",
            params![origin, name, host, unix_days_ago(*days)],
        )
        .unwrap();
    }
}

/// Firefox keeps both databases in WAL mode; the rows stay in the `-wal`
/// file while the returned connections are open, as with a running
/// browser.
fn firefox_profile(dir: &Path) -> (Connection, Connection) {
    let open = |name: &str| {
        let conn = Connection::open(dir.join(name)).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
            .unwrap();
        conn
    };
    let places = open("places.sqlite");
    firefox_places(
        &places,
        &[
            (1, "https://example.com/", Some(4), 0),
            (2, "https://old.example.org/", Some(200), 0),
            (3, "https://saved.example.net/", Some(300), 1),
            (4, "https://never.example.net/", None, 1),
        ],
    );
    let cookies = open("cookies.sqlite");
    firefox_cookies(
        &cookies,
        &[
            (".example.com", "sid", "", 100),
            (".example.org", "pref", "", 200),
            (".example.org", "pref", "^userContextId=2", 200),
            (".ads.example.biz", "uid", "", 150),
        ],
    );
    (places, cookies)
}

fn scan_firefox(dir: &Path) -> ProfilePrivacy {
    scan_profile(BrowserKind::Firefox, &profile(dir), now())
}

#[test]
fn firefox_bookmark_rows_never_count_as_old_history() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path();
    let _open = firefox_profile(dir);
    assert!(dir.join("places.sqlite-wal").exists());
    let result = scan_firefox(dir);
    assert!(result.supported, "{:?}", result.reason);
    assert_eq!(result.history_to_remove, vec![2]);
    assert_eq!(result.old_history_entries, 1);
}

#[test]
fn firefox_cookies_carry_their_origin_attributes() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path();
    let _open = firefox_profile(dir);
    let result = scan_firefox(dir);
    assert_eq!((result.kept, result.old, result.trackers), (1, 2, 1));
    let listed: Vec<(&str, Option<&str>, RemovedCookie)> = result
        .cookies_to_remove
        .iter()
        .map(|k| (k.host.as_str(), k.origin_attributes.as_deref(), k.class))
        .collect();
    assert_eq!(
        listed,
        vec![
            (".ads.example.biz", Some(""), RemovedCookie::Tracker),
            (".example.org", Some(""), RemovedCookie::Old),
            (".example.org", Some("^userContextId=2"), RemovedCookie::Old),
        ]
    );
}

#[test]
fn a_missing_column_makes_the_profile_unsupported() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path();
    chromium_history(dir, &[("https://example.com/", 3)]);
    let conn = Connection::open(dir.join("Cookies")).unwrap();
    conn.execute_batch(
        "CREATE TABLE cookies(host_key TEXT, name TEXT, path TEXT, last_access INTEGER);",
    )
    .unwrap();
    let result = scan_chrome(dir);
    assert!(!result.supported);
    let reason = result.reason.unwrap();
    assert_eq!(reason.kind, PrivacyIssueKind::UnknownSchema);
    assert_eq!(reason.path, Some(dir.join("Cookies")));
    assert!(
        reason.detail.contains("last_access_utc"),
        "{}",
        reason.detail
    );
    assert_eq!((result.kept, result.old, result.trackers), (0, 0, 0));
}

#[test]
fn a_missing_table_or_a_wrong_type_makes_the_profile_unsupported() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path();
    Connection::open(dir.join("places.sqlite"))
        .unwrap()
        .execute_batch("CREATE TABLE moz_historyvisits(id INTEGER);")
        .unwrap();
    let reason = scan_firefox(dir).reason.unwrap();
    assert_eq!(reason.kind, PrivacyIssueKind::UnknownSchema);
    assert!(reason.detail.contains("moz_places"), "{}", reason.detail);

    let other = tempfile::tempdir().unwrap();
    let dir = other.path();
    chromium_history(dir, &[]);
    Connection::open(dir.join("History"))
        .unwrap()
        .execute(
            "INSERT INTO urls(url, last_visit_time) VALUES ('https://example.com/', 'soon')",
            [],
        )
        .unwrap();
    let result = scan_chrome(dir);
    assert_eq!(result.reason.unwrap().kind, PrivacyIssueKind::UnknownSchema);
}

#[test]
fn a_file_that_is_not_a_database_is_unreadable() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path();
    fs::write(dir.join("History"), b"not sqlite at all, just some bytes").unwrap();
    let result = scan_chrome(dir);
    assert!(!result.supported);
    assert_eq!(result.reason.unwrap().kind, PrivacyIssueKind::Unreadable);
}

/// Every file in `dir` with its bytes and modification time.
fn listing(dir: &Path) -> BTreeMap<String, (Vec<u8>, std::time::SystemTime)> {
    fs::read_dir(dir)
        .unwrap()
        .map(|entry| {
            let entry = entry.unwrap();
            let path = entry.path();
            let modified = entry.metadata().unwrap().modified().unwrap();
            (
                entry.file_name().to_string_lossy().into_owned(),
                (fs::read(&path).unwrap(), modified),
            )
        })
        .collect()
}

#[test]
fn the_originals_are_byte_identical_and_nothing_is_created_beside_them() {
    let home = tempfile::tempdir().unwrap();
    let chrome = home.path().join("Default");
    fs::create_dir_all(&chrome).unwrap();
    chromium_history(
        &chrome,
        &[("https://example.com/", 3), ("https://example.org/", 200)],
    );
    chromium_cookies(&chrome.join("Cookies"), &[(".example.net", "uid", 100)]);
    // A journal left beside the database, as Chrome's are.
    fs::write(chrome.join("Cookies-journal"), b"").unwrap();
    let before = listing(&chrome);
    let result = scan_chrome(&chrome);
    assert_eq!((result.trackers, result.old_history_entries), (1, 1));
    assert_eq!(listing(&chrome), before);

    let firefox = home.path().join("firefox");
    fs::create_dir_all(&firefox).unwrap();
    let _open = firefox_profile(&firefox);
    let before = listing(&firefox);
    assert!(before.contains_key("cookies.sqlite-wal") && before.contains_key("cookies.sqlite-shm"));
    let result = scan_firefox(&firefox);
    assert_eq!(result.trackers, 1);
    assert_eq!(listing(&firefox), before);
}

#[test]
fn a_hot_journal_is_rolled_back_on_the_copy_only() {
    let home = tempfile::tempdir().unwrap();
    let dir = home.path();
    chromium_history(
        dir,
        &[("https://example.com/", 3), ("https://example.org/", 200)],
    );
    let history = dir.join("History");
    let journal = dir.join("History-journal");

    // An unfinished transaction that has spilled pages into the database,
    // copied mid-way as a scan of a running browser would see it.
    let writer = Connection::open(&history).unwrap();
    writer.execute_batch("PRAGMA cache_size=1; BEGIN;").unwrap();
    for n in 0..300 {
        writer
            .execute(
                "INSERT INTO urls(url, last_visit_time) VALUES (?1, 1)",
                params![format!("https://p{n}.example.net/{}", "x".repeat(200))],
            )
            .unwrap();
    }
    let crashed = tempfile::tempdir().unwrap();
    let copy = crashed.path().join("History");
    fs::copy(&history, &copy).unwrap();
    fs::copy(&journal, crashed.path().join("History-journal")).unwrap();
    writer.execute_batch("ROLLBACK;").unwrap();
    drop(writer);

    let before = listing(crashed.path());
    let result = scan_chrome(crashed.path());
    assert!(result.supported, "{:?}", result.reason);
    assert_eq!(
        result.history_to_remove,
        vec![2],
        "only committed rows are read"
    );
    assert_eq!(listing(crashed.path()), before);
}
