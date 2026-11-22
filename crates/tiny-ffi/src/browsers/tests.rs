//! Browsers scan tests on made-up fixture homes in temporary folders.
//! Names, emails, sites and extension ids are invented; nothing outside
//! the fixtures is read and nothing is written to a browser.

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::Duration;

use rusqlite::{params, Connection};
use tiny_core::browsers::privacy::RemovedCookie;
use tiny_core::clean::types::Evidence;

use super::*;
use crate::clean::FfiEvidence;
use crate::FfiProgress;

/// 2027-01-15, the "now" every fixture counts back from.
pub(super) const NOW_SECS: u64 = 1_800_000_000;
const DAY_SECS: i64 = 86_400;
/// Seconds from 1601-01-01 (Chromium's epoch) to 1970-01-01.
const CHROMIUM_EPOCH_OFFSET_SECS: i64 = 11_644_473_600;
const EXTENSION: &str = "aaaabbbbccccddddeeeeffffgggghhhh";
pub(super) const CHROME: &str = "Library/Application Support/Google/Chrome";
const BRAVE: &str = "Library/Application Support/BraveSoftware/Brave-Browser";

static NEXT: AtomicU64 = AtomicU64::new(0);

pub(super) fn now() -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(NOW_SECS)
}

/// Microseconds on Chromium's clock, `days` before [`now`].
fn chromium_days_ago(days: i64) -> i64 {
    (NOW_SECS as i64 - days * DAY_SECS + CHROMIUM_EPOCH_OFFSET_SECS) * 1_000_000
}

/// A temporary home, removed on drop.
pub(super) struct Home(PathBuf);

impl Home {
    pub(super) fn new() -> Self {
        let dir = fs::canonicalize(std::env::temp_dir())
            .unwrap()
            .join(format!(
                "tiny-ffi-browsers-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        Home(dir)
    }

    pub(super) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Home {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

pub(super) fn write(path: &Path, contents: &str) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn chmod(path: &Path, mode: u32) {
    fs::set_permissions(path, fs::Permissions::from_mode(mode)).unwrap();
}

/// Chrome with `Default` (signed in, account photo) and `Profile 2`
/// (signed out, built-in avatar). Returns the `Default` folder.
pub(super) fn chrome(home: &Path) -> PathBuf {
    let root = home.join(CHROME);
    for folder in ["Default", "Profile 2"] {
        write(&root.join(folder).join("Preferences"), "{}");
    }
    let default = root.join("Default");
    write(&default.join("Google Profile Picture.png"), "png");
    write(
        &root.join("Local State"),
        r#"{"profile":{"profiles_order":["Default","Profile 2"],"info_cache":{
            "Default":{"name":"Work","user_name":"someone@example.com",
                "avatar_icon":"chrome://theme/IDR_PROFILE_AVATAR_26","use_gaia_picture":true,
                "gaia_picture_file_name":"Google Profile Picture.png",
                "profile_highlight_color":-16033840},
            "Profile 2":{"name":"Home","user_name":"",
                "avatar_icon":"chrome://theme/IDR_PROFILE_AVATAR_4"}}}}"#,
    );
    default
}

/// `urls` rows as `(url, days since the last visit)`, each with one visit.
pub(super) fn history(dir: &Path, rows: &[(&str, i64)]) {
    let conn = Connection::open(dir.join("History")).unwrap();
    conn.execute_batch(
        "CREATE TABLE urls(id INTEGER PRIMARY KEY AUTOINCREMENT, url LONGVARCHAR,
            last_visit_time INTEGER NOT NULL);
         CREATE TABLE visits(id INTEGER PRIMARY KEY, url INTEGER NOT NULL,
            visit_time INTEGER NOT NULL);",
    )
    .unwrap();
    for (url, days) in rows {
        conn.execute(
            "INSERT INTO urls(url, last_visit_time) VALUES (?1, ?2)",
            params![url, chromium_days_ago(*days)],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO visits(url, visit_time) VALUES (last_insert_rowid(), ?1)",
            params![chromium_days_ago(*days)],
        )
        .unwrap();
    }
}

/// `cookies` rows as `(host_key, name, days since the last access)`.
pub(super) fn cookies(dir: &Path, rows: &[(&str, &str, i64)]) {
    let conn = Connection::open(dir.join("Cookies")).unwrap();
    conn.execute_batch(
        "CREATE TABLE cookies(host_key TEXT NOT NULL, name TEXT NOT NULL, path TEXT NOT NULL,
            last_access_utc INTEGER NOT NULL);",
    )
    .unwrap();
    for (host, name, days) in rows {
        conn.execute(
            "INSERT INTO cookies VALUES (?1, ?2, '/', ?3)",
            params![host, name, chromium_days_ago(*days)],
        )
        .unwrap();
    }
}

/// One web-store extension, turned off, last updated 400 days ago.
fn extension(dir: &Path) {
    let chromium = |days: i64| chromium_days_ago(days).to_string();
    write(
        &dir.join("Secure Preferences"),
        &format!(
            r#"{{"extensions":{{"settings":{{"{EXTENSION}":{{"location":1,"from_webstore":true,
                "first_install_time":"{}","last_update_time":"{}",
                "path":"{EXTENSION}/1.0_0","disable_reasons":[1]}}}}}}}}"#,
            chromium(800),
            chromium(400)
        ),
    );
    write(
        &dir.join("Extensions")
            .join(EXTENSION)
            .join("1.0_0/manifest.json"),
        r#"{"name":"Tab Saver","version":"1.0"}"#,
    );
}

/// The `Default` profile with a kept, an old and a tracker cookie, and one
/// old history entry.
pub(super) fn chrome_with_privacy(home: &Path) -> PathBuf {
    let default = chrome(home);
    history(
        &default,
        &[
            ("https://mail.example.com/", 5),
            ("https://old.example.org/a", 120),
        ],
    );
    cookies(
        &default,
        &[
            (".example.com", "SID", 1),
            (".example.org", "pref", 120),
            (".ads.example.net", "uid", 100),
        ],
    );
    extension(&default);
    default
}

/// Answers each process name from a table; a missing name is not running.
#[derive(Default)]
pub(super) struct Probe(HashMap<&'static str, Result<bool, String>>);

impl AppProbe for Probe {
    fn probe(&self, name: &str) -> Result<bool, String> {
        self.0.get(name).cloned().unwrap_or(Ok(false))
    }
}

#[derive(Default)]
pub(super) struct Listener(Mutex<Vec<FfiProgress>>);

impl ProgressListener for Listener {
    fn on_progress(&self, progress: FfiProgress) {
        self.0.lock().unwrap().push(progress);
    }
}

pub(super) fn scan_with(
    session: &TinySession,
    home: &Path,
    probe: &Probe,
) -> Result<FfiBrowsersReport, FfiError> {
    session.scan_browsers(
        home,
        probe,
        now(),
        &CancellationToken::default(),
        &Listener::default(),
    )
}

pub(super) fn scan(session: &TinySession, home: &Path) -> FfiBrowsersReport {
    scan_with(session, home, &Probe::default()).unwrap()
}

pub(super) fn browser(report: &FfiBrowsersReport, kind: FfiBrowserKind) -> &FfiBrowser {
    report.browsers.iter().find(|b| b.kind == kind).unwrap()
}

#[test]
fn a_chrome_profile_comes_back_with_account_avatar_privacy_and_extensions() {
    let home = Home::new();
    let default = chrome_with_privacy(home.path());
    let session = TinySession::new();
    let listener = Listener::default();
    let report = session
        .scan_browsers(
            home.path(),
            &Probe::default(),
            now(),
            &CancellationToken::default(),
            &listener,
        )
        .unwrap();

    let chrome = browser(&report, FfiBrowserKind::Chrome);
    assert_eq!(chrome.name, "Google Chrome");
    assert_eq!(chrome.bundle_id, "com.google.Chrome");
    assert_eq!(chrome.running, Some(false));
    assert_eq!(chrome.issue, None);
    let ids: Vec<&str> = chrome.profiles.iter().map(|p| p.id.as_str()).collect();
    assert_eq!(ids, ["Default", "Profile 2"]);

    let work = &chrome.profiles[0];
    assert_eq!(work.name, "Work");
    assert_eq!(work.user_name.as_deref(), Some("someone@example.com"));
    assert_eq!(
        work.avatar,
        FfiProfileAvatar {
            icon_index: Some(26),
            uses_account_picture: true,
            account_picture: Some(
                default
                    .join("Google Profile Picture.png")
                    .display()
                    .to_string()
            ),
        }
    );
    assert_eq!(work.highlight_color, Some(-16033840_i32 as u32));
    assert_eq!(
        work.privacy,
        FfiProfilePrivacy {
            supported: true,
            issue: None,
            trackers: 1,
            old: 1,
            kept: 1,
            tracker_sites: 1,
            old_sites: 1,
            old_history_entries: 1,
            history_is_empty: false,
        }
    );
    assert_eq!(work.extensions_issue, None);
    let tab_saver = &work.extensions[0];
    assert_eq!(work.extensions.len(), 1);
    assert_eq!(
        (tab_saver.id.as_str(), tab_saver.name.as_str()),
        (EXTENSION, "Tab Saver")
    );
    assert_eq!(tab_saver.version.as_deref(), Some("1.0"));
    assert_eq!(tab_saver.source, FfiExtensionSource::Installed);
    assert!(tab_saver.from_webstore && tab_saver.disabled && tab_saver.stale);
    assert_eq!(tab_saver.updated_at, Some(NOW_SECS as i64 - 400 * DAY_SECS));
    assert_eq!(
        tab_saver.installed_at,
        Some(NOW_SECS as i64 - 800 * DAY_SECS)
    );
    assert!(tab_saver.size_bytes > 0);

    let signed_out = &chrome.profiles[1];
    assert_eq!(signed_out.user_name, None);
    assert_eq!(signed_out.avatar.icon_index, Some(4));
    assert_eq!(signed_out.avatar.account_picture, None);
    assert!(signed_out.privacy.supported);
    assert!(signed_out.privacy.history_is_empty);
    assert!(signed_out.extensions.is_empty());

    let steps = listener.0.lock().unwrap();
    assert_eq!(
        steps.last().map(|p| (p.completed, p.total)),
        Some((2, Some(2)))
    );
    assert!(steps.iter().all(|p| p.operation == "browsers"));
    drop(steps);
    assert!(!session.is_busy(), "the gate is released");
}

/// The removal lists Swift never sees are kept under the scan's generation,
/// exactly as the counts were made.
#[test]
fn the_scan_keeps_what_it_counted_for_the_write() {
    let home = Home::new();
    let default = chrome_with_privacy(home.path());
    let session = TinySession::new();
    let report = scan(&session, home.path());

    let state = session.browsers_state();
    let stored = state
        .consented(report.generation, BrowserKind::Chrome, "Default")
        .unwrap();
    assert_eq!(stored.profile.data_dir, default);
    let keys: Vec<(&str, &str, RemovedCookie)> = stored
        .privacy
        .cookies_to_remove
        .iter()
        .map(|k| (k.host.as_str(), k.name.as_str(), k.class))
        .collect();
    assert_eq!(
        keys,
        [
            (".ads.example.net", "uid", RemovedCookie::Tracker),
            (".example.org", "pref", RemovedCookie::Old),
        ]
    );
    assert_eq!(stored.privacy.history_to_remove, [2]);
    assert_eq!(stored.privacy.cookie_store, Some(default.join("Cookies")));
    assert!(matches!(
        state.consented(report.generation, BrowserKind::Brave, "Default"),
        Err(FfiError::InvalidInput { .. })
    ));
    assert!(matches!(
        state.consented(report.generation, BrowserKind::Chrome, "Profile 9"),
        Err(FfiError::InvalidInput { .. })
    ));
}

#[test]
fn a_new_scan_makes_the_previous_generation_stale() {
    let home = Home::new();
    chrome_with_privacy(home.path());
    let session = TinySession::new();
    assert!(matches!(
        session
            .browsers_state()
            .consented(1, BrowserKind::Chrome, "Default"),
        Err(FfiError::PreviewInvalid { .. })
    ));
    let first = scan(&session, home.path()).generation;
    let second = scan(&session, home.path()).generation;
    assert!(second > first);
    let state = session.browsers_state();
    assert!(matches!(
        state.consented(first, BrowserKind::Chrome, "Default"),
        Err(FfiError::PreviewInvalid { .. })
    ));
    assert!(state
        .consented(second, BrowserKind::Chrome, "Default")
        .is_ok());
}

/// A refused read is an issue on the browser, the profile's privacy or its
/// extensions; the call still succeeds and lists the rest.
#[test]
fn a_refused_read_is_an_issue_not_a_failed_call() {
    let home = Home::new();
    let default = chrome_with_privacy(home.path());
    let brave_state = home.path().join(BRAVE).join("Local State");
    write(&brave_state, "{}");
    let locked = [
        brave_state.clone(),
        default.join("Cookies"),
        default.join("Secure Preferences"),
    ];
    for path in &locked {
        chmod(path, 0o000);
    }
    let session = TinySession::new();
    let outcome = scan_with(&session, home.path(), &Probe::default());
    for path in &locked {
        chmod(path, 0o644);
    }
    let report = outcome.unwrap();

    let brave = browser(&report, FfiBrowserKind::Brave);
    assert!(brave.profiles.is_empty());
    let issue = brave.issue.as_ref().unwrap();
    assert_eq!(issue.kind, FfiBrowserIssueKind::PermissionDenied);
    assert_eq!(issue.path, Some(brave_state.display().to_string()));

    let work = &browser(&report, FfiBrowserKind::Chrome).profiles[0];
    assert!(!work.privacy.supported);
    assert_eq!(
        work.privacy.issue.as_ref().map(|i| i.kind),
        Some(FfiBrowserIssueKind::PermissionDenied)
    );
    assert_eq!((work.privacy.trackers, work.privacy.old), (0, 0));
    assert!(work.extensions.is_empty());
    assert_eq!(
        work.extensions_issue.as_ref().map(|i| i.kind),
        Some(FfiBrowserIssueKind::PermissionDenied)
    );
}

#[test]
fn safari_is_listed_with_privacy_not_supported_and_no_extensions() {
    let home = Home::new();
    fs::create_dir_all(home.path().join("Library/Safari")).unwrap();
    let session = TinySession::new();
    let report = scan(&session, home.path());
    let safari = browser(&report, FfiBrowserKind::Safari);
    assert_eq!(safari.profiles.len(), 1);
    let profile = &safari.profiles[0];
    assert_eq!(profile.id, "");
    assert!(!profile.privacy.supported);
    assert_eq!(
        profile
            .privacy
            .issue
            .as_ref()
            .map(|i| (i.kind, i.path.clone())),
        Some((FfiBrowserIssueKind::NotSupported, None))
    );
    assert!(profile.extensions.is_empty() && profile.extensions_issue.is_none());
}

#[test]
fn running_is_clean_s_name_check_and_unknown_is_none() {
    let home = Home::new();
    chrome(home.path());
    write(&home.path().join(BRAVE).join("Default/Preferences"), "{}");
    let probe = Probe(HashMap::from([
        ("Google Chrome", Ok(true)),
        ("Brave Browser", Err("pgrep timed out".into())),
    ]));
    let session = TinySession::new();
    let report = scan_with(&session, home.path(), &probe).unwrap();
    assert_eq!(browser(&report, FfiBrowserKind::Chrome).running, Some(true));
    assert_eq!(browser(&report, FfiBrowserKind::Brave).running, None);
    // A running browser is still read.
    assert_eq!(browser(&report, FfiBrowserKind::Chrome).profiles.len(), 2);
}

/// Every Firefox edition's process is `firefox`, and `pgrep -x` tells case.
#[test]
fn firefox_runs_by_its_lowercase_process_name() {
    let home = Home::new();
    let firefox = home.path().join("Library/Application Support/Firefox");
    write(
        &firefox.join("profiles.ini"),
        "[Profile0]\nName=default-release\nIsRelative=1\nPath=Profiles/ab.default-release\n",
    );
    write(&firefox.join("Profiles/ab.default-release/prefs.js"), "");
    let probe = Probe(HashMap::from([("firefox", Ok(true))]));
    let session = TinySession::new();
    let report = scan_with(&session, home.path(), &probe).unwrap();
    assert_eq!(
        browser(&report, FfiBrowserKind::Firefox).running,
        Some(true)
    );
}

#[test]
fn a_cancelled_scan_stores_nothing_and_releases_the_gate() {
    let home = Home::new();
    chrome_with_privacy(home.path());
    let session = TinySession::new();
    let first = scan(&session, home.path()).generation;
    let token = CancellationToken::default();
    token.cancel();
    let outcome = session.scan_browsers(
        home.path(),
        &Probe::default(),
        now(),
        &token,
        &Listener::default(),
    );
    assert!(matches!(outcome, Err(FfiError::Cancelled)));
    assert!(!session.is_busy());
    assert!(
        session
            .browsers_state()
            .consented(first, BrowserKind::Chrome, "Default")
            .is_ok(),
        "the previous scan stays the consent basis"
    );
}

#[test]
fn the_scan_is_refused_while_a_write_or_another_browsers_scan_runs() {
    let home = Home::new();
    let session = TinySession::new();
    let write = session.begin().unwrap();
    assert!(matches!(
        scan_with(&session, home.path(), &Probe::default()),
        Err(FfiError::Busy)
    ));
    drop(write);
    let held = session.begin_scan(ScanSlot::Browsers).unwrap();
    assert!(matches!(
        scan_with(&session, home.path(), &Probe::default()),
        Err(FfiError::Busy)
    ));
    drop(held);
    let clean = session.begin_scan(ScanSlot::Clean).unwrap();
    assert!(scan_with(&session, home.path(), &Probe::default()).is_ok());
    drop(clean);
}

#[test]
fn an_empty_home_has_no_browsers() {
    let home = Home::new();
    let report = scan(&TinySession::new(), home.path());
    assert!(report.browsers.is_empty());
}

/// A cache item's evidence names the same kind and profile id the scan
/// lists, so the page can file it under that profile.
#[test]
fn cache_evidence_names_the_scanned_kind_and_profile() {
    let evidence = Evidence::BrowserProfile {
        browser: "Google Chrome".into(),
        browser_kind: BrowserKind::Chrome,
        profile_id: "Profile 2".into(),
        profile_name: "Home".into(),
    };
    assert_eq!(
        FfiEvidence::from(evidence),
        FfiEvidence::BrowserProfile {
            browser: "Google Chrome".into(),
            kind: FfiBrowserKind::Chrome,
            profile_id: "Profile 2".into(),
            profile_name: "Home".into(),
        }
    );
    for kind in BrowserKind::ALL {
        assert_eq!(BrowserKind::from(FfiBrowserKind::from(kind)), kind);
    }
}

/// B12: Clean leaves a running browser's folders out, so the page measures
/// them itself, split by part, by the folders Clean would offer.
#[test]
fn a_running_browser_s_space_is_measured_by_part() {
    let home = Home::new();
    let default = chrome(home.path());
    let caches = home.path().join("Library/Caches/Google/Chrome/Default");
    for (path, bytes) in [
        (caches.join("Cache/Cache_Data/f_000001"), 10_000),
        (caches.join("Code Cache/js/index"), 6_000),
        (caches.join("Storage/ext/x"), 3_000),
        (default.join("GPUCache/data_0"), 2_000),
        (
            default.join("Service Worker/CacheStorage/0a1b/index"),
            9_000,
        ),
        // Never measured: not a cache folder.
        (default.join("Local Storage/leveldb/000003.log"), 50_000),
    ] {
        write(&path, &"x".repeat(bytes));
    }
    let probe = Probe(HashMap::from([("Google Chrome", Ok(true))]));
    let session = TinySession::new();
    let report = scan_with(&session, home.path(), &probe).unwrap();
    let chrome = browser(&report, FfiBrowserKind::Chrome);
    assert_eq!(chrome.running, Some(true));
    let space = &chrome.profiles[0].space;
    // Allocated blocks, as Clean sizes its items: at least the bytes written.
    assert!(space.cache_bytes >= 10_000, "{space:?}");
    assert!(space.code_cache_bytes >= 6_000, "{space:?}");
    assert!(space.other_bytes >= 5_000, "{space:?}");
    assert!(space.offline_bytes >= 9_000, "{space:?}");
    assert!(space.other_bytes < 50_000, "{space:?}");
    assert_eq!(chrome.profiles[1].space, FfiProfileSpace::default());
}

/// An Undo store that cannot record the scan makes the profile's privacy
/// an issue; the call still lists everything else.
#[test]
fn an_unusable_undo_store_is_a_privacy_issue_not_a_failed_call() {
    let home = Home::new();
    chrome_with_privacy(home.path());
    let store = home
        .path()
        .join("Library/Application Support/Tiny/browser-privacy");
    write(&store, "not a folder");
    let session = TinySession::new();
    let report = scan(&session, home.path());
    let work = &browser(&report, FfiBrowserKind::Chrome).profiles[0];
    assert!(!work.privacy.supported);
    let issue = work.privacy.issue.as_ref().unwrap();
    assert_eq!(issue.kind, FfiBrowserIssueKind::Unreadable);
    assert_eq!(issue.path, Some(store.display().to_string()));
    assert_eq!(work.privacy.trackers, 0);
    assert_eq!(work.extensions.len(), 1);
    let state = session.browsers_state();
    let stored = state
        .consented(report.generation, BrowserKind::Chrome, "Default")
        .unwrap();
    assert!(stored.privacy.cookies_to_remove.is_empty());
}
