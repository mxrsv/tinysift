//! Undo of a Privacy write on made-up profiles: every file comes back byte
//! for byte, and Undo refuses a changed file, a running browser and a
//! newer scan, changing nothing.

use rusqlite::Connection;

use super::running::RunningCheck;
use super::tests::now;
use super::tests_write_fixture::*;
use super::*;

fn undo(
    home: &Home,
    kind: BrowserKind,
    id: &str,
    processes: &dyn BrowserProcesses,
) -> Result<PrivacyUndoReport, PrivacyUndoError> {
    let store = home.store();
    let trash = home.trash();
    undo_privacy(kind, &home.profile(), id, &env(processes, &trash, &store))
}

/// Scans as the app does, cleans everything listed, returns the Undo id.
fn clean_all(home: &Home, kind: BrowserKind) -> String {
    let scan = scan_profile_recorded(kind, &home.profile(), now(), &home.store()).unwrap();
    let report = write(home, kind, &consent_all(&scan), &quit(), &home.trash()).unwrap();
    assert!(report.issues.is_empty(), "{:?}", report.issues);
    report.undo_id.expect("an Undo id")
}

#[test]
fn write_then_undo_restores_every_chrome_file_byte_for_byte() {
    let home = Home::new();
    chrome_profile(&home);
    let before = home.listing();
    let id = clean_all(&home, BrowserKind::Chrome);
    let after_write = home.listing();
    assert_ne!(after_write, before);
    assert!(!after_write.contains_key("Visited Links"));

    let report = undo(&home, BrowserKind::Chrome, &id, &quit()).unwrap();
    assert_eq!(home.listing(), before);
    let mut restored: Vec<_> = report
        .restored
        .iter()
        .map(|p| p.file_name().unwrap().to_string_lossy().into_owned())
        .collect();
    restored.sort();
    assert_eq!(
        restored,
        [
            "Cookies",
            "Favicons",
            "History",
            "Shortcuts",
            "Top Sites",
            "Visited Links"
        ]
    );
    // The backup stays in the Trash; the record is consumed.
    assert!(home.trashed().iter().any(|n| n == "Visited Links"));
    assert_eq!(
        undo(&home, BrowserKind::Chrome, &id, &quit()).unwrap_err(),
        PrivacyUndoError::NoRecord
    );
}

#[test]
fn a_firefox_profile_with_a_wal_round_trips_byte_for_byte() {
    let home = Home::new();
    firefox_profile(&home, true);
    let before = home.listing();
    assert!(
        before.contains_key("places.sqlite-wal"),
        "{:?}",
        before.keys()
    );
    let id = clean_all(&home, BrowserKind::Firefox);
    assert_ne!(home.listing(), before);
    undo(&home, BrowserKind::Firefox, &id, &quit()).unwrap();
    assert_eq!(home.listing(), before);
}

#[test]
fn undo_refuses_a_file_the_browser_changed_after_the_write() {
    let home = Home::new();
    chrome_profile(&home);
    let id = clean_all(&home, BrowserKind::Chrome);
    Connection::open(home.dir().join("Cookies"))
        .unwrap()
        .execute_batch("DELETE FROM cookies")
        .unwrap();
    let changed = home.listing();
    let error = undo(&home, BrowserKind::Chrome, &id, &quit()).unwrap_err();
    assert_eq!(error, PrivacyUndoError::Changed(home.dir().join("Cookies")));
    assert_eq!(home.listing(), changed);
}

#[test]
fn undo_refuses_while_the_browser_or_a_helper_runs() {
    let home = Home::new();
    chrome_profile(&home);
    let id = clean_all(&home, BrowserKind::Chrome);
    let after_write = home.listing();
    let named = running_named("Google Chrome");
    let quiet = Bundles(Vec::new());
    let helper = Bundles(vec!["com.google.Chrome.framework.AlertNotificationService"]);
    let none = crate::clean::process::test_support::MockChecker::none();
    for check in [
        RunningCheck {
            names: &named,
            bundles: &quiet,
        },
        RunningCheck {
            names: &none,
            bundles: &helper,
        },
    ] {
        let error = undo(&home, BrowserKind::Chrome, &id, &check).unwrap_err();
        assert!(matches!(error, PrivacyUndoError::Running(_)), "{error:?}");
        assert_eq!(home.listing(), after_write);
    }
    // Once it quits, the same Undo still applies.
    undo(&home, BrowserKind::Chrome, &id, &quit()).unwrap();
}

#[test]
fn undo_refuses_after_a_newer_scan() {
    let home = Home::new();
    chrome_profile(&home);
    let id = clean_all(&home, BrowserKind::Chrome);
    let after_write = home.listing();
    scan_profile_recorded(BrowserKind::Chrome, &home.profile(), now(), &home.store()).unwrap();
    assert_eq!(
        undo(&home, BrowserKind::Chrome, &id, &quit()).unwrap_err(),
        PrivacyUndoError::Expired
    );
    assert_eq!(home.listing(), after_write);
    assert_eq!(
        undo(&home, BrowserKind::Chrome, &id, &quit()).unwrap_err(),
        PrivacyUndoError::NoRecord
    );
}

#[test]
fn undo_refuses_another_profile_or_an_unknown_id() {
    let home = Home::new();
    chrome_profile(&home);
    let id = clean_all(&home, BrowserKind::Chrome);
    assert_eq!(
        undo(&home, BrowserKind::Chrome, "not-the-id", &quit()).unwrap_err(),
        PrivacyUndoError::NoRecord
    );
    assert_eq!(
        undo(&home, BrowserKind::Brave, &id, &quit()).unwrap_err(),
        PrivacyUndoError::NoRecord
    );
}

#[test]
fn undo_refuses_a_backup_changed_in_the_trash() {
    let home = Home::new();
    chrome_profile(&home);
    let id = clean_all(&home, BrowserKind::Chrome);
    let after_write = home.listing();
    let folder = std::fs::read_dir(home.trash_dir())
        .unwrap()
        .flatten()
        .find(|e| e.file_type().unwrap().is_dir())
        .unwrap()
        .path();
    std::fs::write(folder.join("History"), b"not the original").unwrap();
    let error = undo(&home, BrowserKind::Chrome, &id, &quit()).unwrap_err();
    assert!(matches!(error, PrivacyUndoError::Backup(_)), "{error:?}");
    assert_eq!(home.listing(), after_write);
}

/// Quit at the first check, running from the `after`-th on.
struct StartsLater {
    calls: std::sync::atomic::AtomicUsize,
    after: usize,
}

impl BrowserProcesses for StartsLater {
    fn running(&self, _kind: BrowserKind) -> Result<bool, String> {
        let call = self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(call >= self.after)
    }
}

#[test]
fn a_browser_started_during_the_clean_refuses_and_keeps_the_earlier_undo() {
    let home = Home::new();
    chrome_profile(&home);
    let before = home.listing();
    let scan =
        scan_profile_recorded(BrowserKind::Chrome, &home.profile(), now(), &home.store()).unwrap();
    let cookies_only = PrivacyConsent {
        cookies: scan.cookies_to_remove.clone(),
        history: Vec::new(),
    };
    let first = write(
        &home,
        BrowserKind::Chrome,
        &cookies_only,
        &quit(),
        &home.trash(),
    )
    .unwrap()
    .undo_id
    .unwrap();
    let after_first = home.listing();

    // The browser starts between the backup and the edit.
    let starts = StartsLater {
        calls: Default::default(),
        after: 1,
    };
    let error = write(
        &home,
        BrowserKind::Chrome,
        &consent_all(&scan),
        &starts,
        &home.trash(),
    )
    .unwrap_err();
    assert!(matches!(error, PrivacyWriteError::Running(_)), "{error:?}");
    assert_eq!(starts.calls.load(std::sync::atomic::Ordering::SeqCst), 2);
    assert_eq!(home.listing(), after_first);

    undo(&home, BrowserKind::Chrome, &first, &quit()).unwrap();
    assert_eq!(home.listing(), before);
}
