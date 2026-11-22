//! Privacy clean and Undo tests on made-up fixture homes. The Trash is the
//! fixture's own `.Trash` folder and the Undo store the fixture home's, so
//! nothing touches the real Trash, the real Undo store or `~/Library`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{params, Connection};
use tiny_core::browsers::privacy::BrowserProcesses;
use tiny_core::browsers::BrowserKind;
use tiny_core::clean::finder_trash::{Trash, TrashError};

use super::tests::{chrome_with_privacy, now, scan, Home};
use super::*;
use crate::session::ScanSlot;

/// Moves into the fixture's `.Trash` and says where, as the native Trash
/// does.
struct FolderTrash(PathBuf);

impl FolderTrash {
    fn of(home: &Home) -> Self {
        FolderTrash(home.path().join(".Trash"))
    }

    fn names(&self) -> Vec<String> {
        fs::read_dir(&self.0).map_or_else(
            |_| Vec::new(),
            |entries| {
                entries
                    .flatten()
                    .map(|e| e.file_name().to_string_lossy().into_owned())
                    .collect()
            },
        )
    }
}

impl Trash for FolderTrash {
    fn move_to_trash(&self, path: &Path) -> Result<(), TrashError> {
        self.move_to_trash_reporting(path).map(|_| ())
    }

    fn move_to_trash_reporting(&self, path: &Path) -> Result<Option<PathBuf>, TrashError> {
        fs::create_dir_all(&self.0).map_err(|e| TrashError::Failed(e.to_string()))?;
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let mut dest = self.0.join(&name);
        let mut n = 2;
        while dest.symlink_metadata().is_ok() {
            dest = self.0.join(format!("{name} {n}"));
            n += 1;
        }
        fs::rename(path, &dest).map_err(|e| TrashError::Failed(e.to_string()))?;
        Ok(Some(dest))
    }
}

/// The browser runs, does not, or the check cannot tell.
struct Processes(Result<bool, String>);

impl BrowserProcesses for Processes {
    fn running(&self, _kind: BrowserKind) -> Result<bool, String> {
        self.0.clone()
    }
}

const QUIT: Processes = Processes(Ok(false));

/// Answers "not running", noting whether a Browsers scan could claim its
/// slot at that moment, which is in the middle of the write.
struct GateWatch<'s> {
    session: &'s TinySession,
    scan_refused: Mutex<Vec<bool>>,
}

impl BrowserProcesses for GateWatch<'_> {
    fn running(&self, _kind: BrowserKind) -> Result<bool, String> {
        let refused = matches!(
            self.session.begin_scan(ScanSlot::Browsers),
            Err(FfiError::Busy)
        );
        self.scan_refused.lock().unwrap().push(refused);
        Ok(false)
    }
}

fn clean(
    session: &TinySession,
    home: &Home,
    generation: u64,
    processes: &dyn BrowserProcesses,
) -> Result<FfiBrowserPrivacyReport, FfiError> {
    session.clean_browser_privacy(
        home.path(),
        generation,
        BrowserKind::Chrome,
        "Default",
        processes,
        &FolderTrash::of(home),
        now(),
    )
}

fn undo(
    session: &TinySession,
    home: &Home,
    undo_id: &str,
    processes: &dyn BrowserProcesses,
) -> Result<FfiBrowserUndoReport, FfiError> {
    session.undo_browser_privacy(
        home.path(),
        BrowserKind::Chrome,
        "Default",
        undo_id,
        processes,
        &FolderTrash::of(home),
    )
}

/// Every file directly in `dir`, by name, with its bytes.
fn listing(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    fs::read_dir(dir)
        .unwrap()
        .flatten()
        .filter(|e| e.file_type().unwrap().is_file())
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                fs::read(e.path()).unwrap(),
            )
        })
        .collect()
}

fn cookie_rows(dir: &Path) -> Vec<String> {
    let conn = Connection::open(dir.join("Cookies")).unwrap();
    let mut statement = conn
        .prepare("SELECT host_key || ' ' || name FROM cookies ORDER BY 1")
        .unwrap();
    let rows = statement.query_map([], |r| r.get(0)).unwrap();
    rows.map(Result::unwrap).collect()
}

fn url_rows(dir: &Path) -> Vec<String> {
    let conn = Connection::open(dir.join("History")).unwrap();
    let mut statement = conn.prepare("SELECT url FROM urls ORDER BY 1").unwrap();
    let rows = statement.query_map([], |r| r.get(0)).unwrap();
    rows.map(Result::unwrap).collect()
}

fn refusal<T: std::fmt::Debug>(outcome: Result<T, FfiError>) -> FfiBrowserPrivacyRefusal {
    match outcome {
        Err(FfiError::BrowserPrivacyRefused { refusal }) => refusal,
        other => panic!("expected a refusal, got {other:?}"),
    }
}

/// The clean takes the stored keys and rows only: a tracker cookie and an
/// old page that appeared after the scan stay, and so does the kept cookie.
#[test]
fn a_clean_removes_exactly_what_the_scan_stored() {
    let home = Home::new();
    let default = chrome_with_privacy(home.path());
    let session = TinySession::new();
    let generation = scan(&session, home.path()).generation;
    Connection::open(default.join("Cookies"))
        .unwrap()
        .execute(
            "INSERT INTO cookies VALUES ('.late.example.biz', 'uid', '/', 0)",
            [],
        )
        .unwrap();
    Connection::open(default.join("History"))
        .unwrap()
        .execute(
            "INSERT INTO urls(url, last_visit_time) VALUES (?1, 0)",
            params!["https://late.example.org/"],
        )
        .unwrap();

    let report = clean(&session, &home, generation, &QUIT).unwrap();
    assert_eq!(report.profile_id, "Default");
    assert_eq!(
        (report.removed_cookies, report.removed_history_entries),
        (2, 1)
    );
    assert_eq!(report.skipped_cookies, FfiPrivacySkips::default());
    assert_eq!(report.skipped_history, FfiPrivacySkips::default());
    assert!(report.undo_id.is_some());
    assert!(report.issues.is_empty(), "{:?}", report.issues);
    let backup = PathBuf::from(report.backup_path.unwrap());
    assert!(backup.starts_with(home.path().join(".Trash")));
    assert!(backup.join("Cookies").is_file() && backup.join("History").is_file());

    assert_eq!(
        cookie_rows(&default),
        [".example.com SID", ".late.example.biz uid"]
    );
    assert_eq!(
        url_rows(&default),
        ["https://late.example.org/", "https://mail.example.com/"]
    );
    assert!(!session.is_busy(), "the gate is released");

    // The consent is spent: the same generation cannot clean twice.
    assert!(matches!(
        clean(&session, &home, generation, &QUIT),
        Err(FfiError::PreviewInvalid { .. })
    ));
}

#[test]
fn a_stale_generation_or_unknown_profile_changes_nothing() {
    let home = Home::new();
    let default = chrome_with_privacy(home.path());
    let session = TinySession::new();
    let first = scan(&session, home.path()).generation;
    let second = scan(&session, home.path()).generation;
    let before = listing(&default);

    assert!(matches!(
        clean(&session, &home, first, &QUIT),
        Err(FfiError::PreviewInvalid { .. })
    ));
    let unknown = session.clean_browser_privacy(
        home.path(),
        second,
        BrowserKind::Chrome,
        "Profile 9",
        &QUIT,
        &FolderTrash::of(&home),
        now(),
    );
    assert!(matches!(unknown, Err(FfiError::InvalidInput { .. })));
    assert_eq!(listing(&default), before);
    assert!(FolderTrash::of(&home).names().is_empty());
}

/// A running or unknown browser is refused with nothing touched, and the
/// same scan cleans once the browser has quit.
#[test]
fn a_running_browser_is_refused_and_the_consent_stays_usable() {
    let home = Home::new();
    let default = chrome_with_privacy(home.path());
    let session = TinySession::new();
    let generation = scan(&session, home.path()).generation;
    let before = listing(&default);

    assert_eq!(
        refusal(clean(&session, &home, generation, &Processes(Ok(true)))),
        FfiBrowserPrivacyRefusal::Running {
            kind: FfiBrowserKind::Chrome
        }
    );
    assert_eq!(
        refusal(clean(
            &session,
            &home,
            generation,
            &Processes(Err("pgrep timed out".into()))
        )),
        FfiBrowserPrivacyRefusal::RunningUnknown {
            kind: FfiBrowserKind::Chrome,
            detail: "pgrep timed out".into()
        }
    );
    assert_eq!(listing(&default), before);
    assert!(FolderTrash::of(&home).names().is_empty());

    let report = clean(&session, &home, generation, &QUIT).unwrap();
    assert_eq!(report.removed_cookies, 2);
}

#[test]
fn undo_puts_every_byte_back_and_runs_once() {
    let home = Home::new();
    let default = chrome_with_privacy(home.path());
    let session = TinySession::new();
    let generation = scan(&session, home.path()).generation;
    let before = listing(&default);

    let undo_id = clean(&session, &home, generation, &QUIT)
        .unwrap()
        .undo_id
        .unwrap();
    assert_ne!(listing(&default), before);
    assert_eq!(
        refusal(undo(&session, &home, &undo_id, &Processes(Ok(true)))),
        FfiBrowserPrivacyRefusal::Running {
            kind: FfiBrowserKind::Chrome
        }
    );

    let report = undo(&session, &home, &undo_id, &QUIT).unwrap();
    assert_eq!(report.profile_id, "Default");
    assert!(report.restored_files >= 2, "{report:?}");
    assert_eq!(listing(&default), before);
    assert_eq!(
        refusal(undo(&session, &home, &undo_id, &QUIT)),
        FfiBrowserPrivacyRefusal::NothingToUndo
    );
}

/// B25: a Browsers scan ends the Undo of the clean before it.
#[test]
fn a_rescan_ends_the_undo() {
    let home = Home::new();
    let default = chrome_with_privacy(home.path());
    let session = TinySession::new();
    let generation = scan(&session, home.path()).generation;
    let undo_id = clean(&session, &home, generation, &QUIT)
        .unwrap()
        .undo_id
        .unwrap();
    let cleaned = listing(&default);
    scan(&session, home.path());
    assert_eq!(
        refusal(undo(&session, &home, &undo_id, &QUIT)),
        FfiBrowserPrivacyRefusal::Expired
    );
    assert_eq!(listing(&default), cleaned);
}

/// The clean and the Undo hold the write gate from start to end: a
/// Browsers scan cannot start while they run, and they cannot start while
/// one does.
#[test]
fn the_clean_and_undo_hold_the_write_gate() {
    let home = Home::new();
    chrome_with_privacy(home.path());
    let session = TinySession::new();
    let generation = scan(&session, home.path()).generation;

    let held = session.begin_scan(ScanSlot::Browsers).unwrap();
    assert!(matches!(
        clean(&session, &home, generation, &QUIT),
        Err(FfiError::Busy)
    ));
    drop(held);

    let watch = GateWatch {
        session: &session,
        scan_refused: Mutex::new(Vec::new()),
    };
    let undo_id = clean(&session, &home, generation, &watch)
        .unwrap()
        .undo_id
        .unwrap();
    let held = session.begin_scan(ScanSlot::Clean).unwrap();
    assert!(matches!(
        undo(&session, &home, &undo_id, &QUIT),
        Err(FfiError::Busy)
    ));
    drop(held);
    undo(&session, &home, &undo_id, &watch).unwrap();

    let seen = watch.scan_refused.lock().unwrap();
    assert!(seen.len() >= 4, "checked at each running check: {seen:?}");
    assert!(seen.iter().all(|refused| *refused), "{seen:?}");
    drop(seen);
    assert!(!session.is_busy());
}

#[test]
fn safari_is_refused_as_not_supported() {
    let home = Home::new();
    fs::create_dir_all(home.path().join("Library/Safari")).unwrap();
    let session = TinySession::new();
    let generation = scan(&session, home.path()).generation;
    let outcome = session.clean_browser_privacy(
        home.path(),
        generation,
        BrowserKind::Safari,
        "",
        &QUIT,
        &FolderTrash::of(&home),
        now(),
    );
    match refusal(outcome) {
        FfiBrowserPrivacyRefusal::Unsupported { issue } => {
            assert_eq!(issue.kind, FfiBrowserIssueKind::NotSupported)
        }
        other => panic!("{other:?}"),
    }
}

#[test]
fn a_profile_with_nothing_to_remove_changes_nothing() {
    let home = Home::new();
    let default = chrome_with_privacy(home.path());
    let session = TinySession::new();
    let generation = scan(&session, home.path()).generation;
    let profile = default.parent().unwrap().join("Profile 2");
    let before = listing(&profile);
    let report = session
        .clean_browser_privacy(
            home.path(),
            generation,
            BrowserKind::Chrome,
            "Profile 2",
            &Processes(Ok(true)),
            &FolderTrash::of(&home),
            now(),
        )
        .unwrap();
    assert_eq!((report.removed_cookies, report.undo_id), (0, None));
    assert_eq!(listing(&profile), before);
}

#[test]
fn every_write_and_undo_error_maps_to_its_case() {
    use std::path::PathBuf;
    use tiny_core::browsers::privacy::{PrivacyUndoError as U, PrivacyWriteError as W};
    use FfiBrowserPrivacyRefusal as R;
    let kind = BrowserKind::Brave;
    let as_refusal = |error| refusal::<()>(Err(error));
    assert_eq!(
        as_refusal(write::write_error(kind, W::Changed("moved".into()))),
        R::Changed {
            detail: "moved".into()
        }
    );
    assert_eq!(
        as_refusal(write::write_error(kind, W::Store("locked".into()))),
        R::UndoStore {
            detail: "locked".into()
        }
    );
    assert_eq!(
        as_refusal(write::write_error(kind, W::Backup("full".into()))),
        R::Backup {
            detail: "full".into()
        }
    );
    assert!(matches!(
        write::write_error(
            kind,
            W::Write {
                detail: "disk I/O".into(),
                undo_id: Some("u1".into())
            }
        ),
        FfiError::BrowserPrivacyFailed { undo_id: Some(id), .. } if id == "u1"
    ));
    assert_eq!(
        as_refusal(write::undo_error(kind, U::Unfinished)),
        R::Unfinished
    );
    assert_eq!(
        as_refusal(write::undo_error(
            kind,
            U::Changed(PathBuf::from("/p/History"))
        )),
        R::ChangedSinceClean {
            path: "/p/History".into()
        }
    );
    assert_eq!(
        as_refusal(write::undo_error(kind, U::Running("Brave Browser".into()))),
        R::Running {
            kind: FfiBrowserKind::Brave
        }
    );
    assert!(matches!(
        write::undo_error(kind, U::Failed("partway".into())),
        FfiError::Operation { .. }
    ));
}
