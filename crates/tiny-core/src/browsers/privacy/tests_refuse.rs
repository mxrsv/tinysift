//! What the Privacy write refuses, changing nothing: a running browser or
//! helper, an unknown schema, a database another connection holds, Safari,
//! and an empty consent.

use std::fs;

use rusqlite::Connection;

use super::running::RunningCheck;
use super::tests_write_fixture::*;
use super::*;
use crate::clean::process::test_support::{FailingProbe, MockChecker};

#[test]
fn a_running_browser_or_helper_refuses_and_changes_nothing() {
    let home = Home::new();
    chrome_profile(&home);
    let consent = consent_all(&scan_of(&home, BrowserKind::Chrome));
    let before = home.listing();
    let named = running_named("Google Chrome");
    let helper = Bundles(vec!["com.google.Chrome.helper.renderer"]);
    let none = MockChecker::none();
    let quiet = Bundles(Vec::new());
    let cases: [(RunningCheck<'_>, bool); 3] = [
        (
            RunningCheck {
                names: &named,
                bundles: &quiet,
            },
            true,
        ),
        (
            RunningCheck {
                names: &none,
                bundles: &helper,
            },
            true,
        ),
        (
            RunningCheck {
                names: &FailingProbe,
                bundles: &quiet,
            },
            false,
        ),
    ];
    for (check, known) in cases {
        let error = write(&home, BrowserKind::Chrome, &consent, &check, &home.trash()).unwrap_err();
        match error {
            PrivacyWriteError::Running(_) => assert!(known),
            PrivacyWriteError::RunningUnknown { .. } => assert!(!known),
            other => panic!("{other:?}"),
        }
        assert_eq!(home.listing(), before);
        assert!(home.trashed().is_empty());
    }
    // Another browser's helper does not hold Chrome.
    let brave = Bundles(vec!["com.brave.Browser.helper"]);
    let check = RunningCheck {
        names: &none,
        bundles: &brave,
    };
    assert!(write(&home, BrowserKind::Chrome, &consent, &check, &home.trash()).is_ok());
}

#[test]
fn an_unknown_schema_is_refused_before_anything_changes() {
    // A leftover table without the column the edit keys on.
    let home = Home::new();
    chrome_profile(&home);
    fs::remove_file(home.dir().join("Top Sites")).unwrap();
    leftover_db(
        &home.dir().join("Top Sites"),
        "CREATE TABLE top_sites(page LONGVARCHAR PRIMARY KEY);",
        "INSERT INTO top_sites(page) VALUES (?1)",
        &[KEEP_URL],
    );
    let before = home.listing();
    let scan = scan_of(&home, BrowserKind::Chrome);
    let error = write(
        &home,
        BrowserKind::Chrome,
        &consent_all(&scan),
        &quit(),
        &home.trash(),
    )
    .unwrap_err();
    match error {
        PrivacyWriteError::Unsupported(issue) => {
            assert_eq!(issue.kind, PrivacyIssueKind::UnknownSchema)
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(home.listing(), before);
    assert!(home.trashed().is_empty());

    // History without `visits`: the scan reads it, the edit refuses it.
    let home = Home::new();
    chrome_profile(&home);
    Connection::open(home.dir().join("History"))
        .unwrap()
        .execute_batch("DROP TABLE visits")
        .unwrap();
    let before = home.listing();
    let scan = scan_of(&home, BrowserKind::Chrome);
    let error = write(
        &home,
        BrowserKind::Chrome,
        &consent_all(&scan),
        &quit(),
        &home.trash(),
    )
    .unwrap_err();
    assert!(
        matches!(error, PrivacyWriteError::Unsupported(_)),
        "{error:?}"
    );
    assert_eq!(home.listing(), before);
    assert!(home.trashed().is_empty());
}

#[test]
fn safari_is_refused() {
    let home = Home::new();
    let consent = PrivacyConsent::default();
    let error = write(&home, BrowserKind::Safari, &consent, &quit(), &home.trash()).unwrap_err();
    assert!(
        matches!(error, PrivacyWriteError::NotSupported(_)),
        "{error:?}"
    );
}

#[test]
fn nothing_consented_writes_nothing() {
    let home = Home::new();
    chrome_profile(&home);
    let before = home.listing();
    let report = write(
        &home,
        BrowserKind::Chrome,
        &PrivacyConsent::default(),
        &quit(),
        &home.trash(),
    )
    .unwrap();
    assert_eq!((report.backup, report.undo_id), (None, None));
    assert_eq!(home.listing(), before);
    assert!(home.trashed().is_empty());
}

/// A database the browser holds (it started after the last running check)
/// fails the edit before its commit: nothing of ours changed, so the write
/// is a refusal, keeps no Undo record and the same consent works later.
#[test]
fn a_database_held_by_another_connection_refuses_and_keeps_no_record() {
    let home = Home::new();
    chrome_profile(&home);
    let before = home.listing();
    let consent = consent_all(&scan_of(&home, BrowserKind::Chrome));

    let holder = Connection::open(home.dir().join("Cookies")).unwrap();
    holder.execute_batch("BEGIN").unwrap();
    let _: i64 = holder
        .query_row("SELECT count(*) FROM cookies", [], |r| r.get(0))
        .unwrap();
    let error = write(&home, BrowserKind::Chrome, &consent, &quit(), &home.trash()).unwrap_err();
    assert!(
        matches!(&error, PrivacyWriteError::Changed(detail) if detail.contains("locked")),
        "{error:?}"
    );
    assert_eq!(home.listing(), before);
    let store = home.store();
    let document = store.lock().unwrap().load().unwrap();
    assert!(
        document
            .entry(BrowserKind::Chrome, &home.profile())
            .is_none_or(|e| e.record.is_none()),
        "{document:?}"
    );
    holder.execute_batch("COMMIT").unwrap();
    drop(holder);

    let report = write(&home, BrowserKind::Chrome, &consent, &quit(), &home.trash()).unwrap();
    assert!(report.removed_cookies > 0);
    assert!(report.undo_id.is_some());
}
