//! Scan, move and undo on temp-dir fixtures: a fake Applications folder, a
//! fake Trash that renames into the fixture's own `.Trash`, and a temp
//! saved-rules store. Nothing reads the real home, Applications or Trash.

use std::collections::HashMap;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use tempfile::TempDir;

use super::app_version::AppVersions;
use super::move_run::{move_to_trash, MoveContext, MoveOutcome, PlannedMove};
use super::undo::{undo_moves, UndoItem, UndoOutcome};
use super::*;
use crate::clean::checked_execute::{SkipReason, StopReason};
use crate::clean::failure_cause::FailureCause;
use crate::clean::finder_trash::{Trash, TrashError};
use crate::clean::protection::RuleScope;
use crate::clean::protection_store::ProtectionStore;

const MB: usize = 1 << 20;

/// `home/Downloads`, `home/Desktop`, `apps` (the system Applications folder)
/// and `home/Applications`, in a canonical temp dir (the move checks refuse
/// a path that reaches its root through a symlink such as macOS's `/var`).
struct Fixture {
    _dir: TempDir,
    home: PathBuf,
    apps: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let home = base.join("home");
        let apps = base.join("apps");
        for folder in ["Downloads", "Desktop", "Applications", ".Trash"] {
            fs::create_dir_all(home.join(folder)).unwrap();
        }
        fs::create_dir_all(&apps).unwrap();
        Self {
            _dir: dir,
            home,
            apps,
        }
    }

    fn roots(&self) -> InstallerRoots {
        InstallerRoots {
            folders: vec![self.home.join("Downloads"), self.home.join("Desktop")],
            applications: vec![self.apps.clone(), self.home.join("Applications")],
        }
    }

    fn app(&self, name: &str) -> PathBuf {
        let path = self.apps.join(format!("{name}.app"));
        fs::create_dir_all(path.join("Contents")).unwrap();
        path
    }

    fn download(&self, name: &str, megabytes: usize) -> PathBuf {
        write(&self.home.join("Downloads").join(name), megabytes)
    }

    fn trash_dir(&self) -> PathBuf {
        self.home.join(".Trash")
    }
}

fn write(path: &Path, megabytes: usize) -> PathBuf {
    let mut file = fs::File::create(path).unwrap();
    // Not all zeros, so nothing compresses or shares blocks.
    let block: Vec<u8> = (0..MB).map(|i| (i % 251) as u8).collect();
    for _ in 0..megabytes {
        file.write_all(&block).unwrap();
    }
    path.to_path_buf()
}

#[derive(Default)]
struct Versions(HashMap<PathBuf, &'static str>);

impl AppVersions for Versions {
    fn version(&self, app: &Path) -> Option<String> {
        self.0.get(app).map(|v| (*v).to_string())
    }
}

/// Scans with a 1 MB threshold, so fixtures stay small.
fn scan(fixture: &Fixture, versions: &Versions) -> InstallerScan {
    scan_installers_above(MB as u64, &fixture.roots(), versions, None)
}

fn names(scan: &InstallerScan) -> Vec<String> {
    scan.files
        .iter()
        .map(|f| f.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

// ---------- scan ----------

#[test]
fn an_installer_whose_app_is_installed_lands_in_the_group_with_the_apps_name() {
    let fixture = Fixture::new();
    let app = fixture.app("Cap");
    fixture.download("Cap-1.4.2.dmg", 2);
    let found = scan(&fixture, &Versions::default());
    assert_eq!(found.files.len(), 1);
    let file = &found.files[0];
    assert_eq!(file.group, Group::Installers);
    assert_eq!(file.group.id(), "installers");
    assert_eq!(file.name, "Cap installer");
    assert_eq!(file.reason, "Cap is installed in Applications");
    assert_eq!(file.matched_app.as_deref(), Some(app.as_path()));
    assert!(file.frees.bytes >= 2 * MB as u64 && file.frees.exact);
}

#[test]
fn an_installer_whose_app_is_not_installed_is_not_this_groups() {
    let fixture = Fixture::new();
    fixture.app("Cap");
    fixture.download("Blender-4.2.dmg", 2);
    assert!(scan(&fixture, &Versions::default()).files.is_empty());
}

#[test]
fn version_and_architecture_tokens_do_not_stop_a_match() {
    let fixture = Fixture::new();
    fixture.app("Visual Studio Code");
    fixture.app("Cap");
    fixture.app("Tool");
    fixture.app("TinySift");
    for name in [
        "visual-studio-code-darwin-arm64.dmg",
        "Cap_arm64.pkg",
        "tool v1.14.1.xip",
        "TINYSIFT-0.7.2.DMG",
    ] {
        fixture.download(name, 2);
    }
    assert_eq!(scan(&fixture, &Versions::default()).files.len(), 4);
}

#[test]
fn a_newer_installed_version_is_said_only_when_both_versions_parse() {
    let fixture = Fixture::new();
    let cap = fixture.app("Cap");
    let tool = fixture.app("Tool");
    let notes = fixture.app("Notes");
    fixture.app("Plain");
    fixture.download("Cap-1.4.2.dmg", 2);
    fixture.download("Tool-2.0.dmg", 3);
    fixture.download("Notes-1.0.dmg", 4);
    fixture.download("Plain-1.0.dmg", 5);
    let versions = Versions(HashMap::from([
        (cap, "1.5.0"),
        (tool, "2.0.0"),
        (notes, "beta"),
    ]));
    let found = scan(&fixture, &versions);
    let reason = |name: &str| {
        found
            .files
            .iter()
            .find(|f| f.path.file_name().unwrap() == name)
            .unwrap()
            .reason
            .clone()
    };
    assert_eq!(
        reason("Cap-1.4.2.dmg"),
        "A newer Cap is installed in Applications"
    );
    assert_eq!(
        reason("Tool-2.0.dmg"),
        "Tool is installed in Applications",
        "equal"
    );
    assert_eq!(
        reason("Notes-1.0.dmg"),
        "Notes is installed in Applications",
        "unparsed"
    );
    assert_eq!(
        reason("Plain-1.0.dmg"),
        "Plain is installed in Applications",
        "unknown"
    );
}

#[test]
fn an_older_installed_app_is_not_called_newer() {
    let fixture = Fixture::new();
    let cap = fixture.app("Cap");
    fixture.download("Cap-2.0.dmg", 2);
    let found = scan(&fixture, &Versions(HashMap::from([(cap, "1.9")])));
    assert_eq!(found.files[0].reason, "Cap is installed in Applications");
}

#[test]
fn an_app_in_the_users_applications_folder_counts_too() {
    let fixture = Fixture::new();
    fs::create_dir_all(fixture.home.join("Applications/Cap.app")).unwrap();
    fixture.download("Cap.dmg", 2);
    assert_eq!(scan(&fixture, &Versions::default()).files.len(), 1);
}

#[test]
fn downloads_and_desktop_are_read_one_level_deep_and_only_real_installers() {
    let fixture = Fixture::new();
    fixture.app("Cap");
    fixture.download("Cap.dmg", 2);
    write(&fixture.home.join("Desktop/Cap-2.pkg"), 3);
    // Not listed: wrong extension, a subfolder, a symlink, a folder named like one.
    fixture.download("Cap.zip", 2);
    fs::create_dir(fixture.home.join("Downloads/old")).unwrap();
    write(&fixture.home.join("Downloads/old/Cap-0.1.dmg"), 2);
    std::os::unix::fs::symlink(
        fixture.home.join("Downloads/Cap.dmg"),
        fixture.home.join("Downloads/Cap-link.dmg"),
    )
    .unwrap();
    fs::create_dir(fixture.home.join("Downloads/Cap-3.pkg")).unwrap();
    let found = scan(&fixture, &Versions::default());
    assert_eq!(names(&found), vec!["Cap-2.pkg", "Cap.dmg"], "largest first");
}

#[test]
fn a_missing_folder_is_skipped_and_an_unreadable_one_is_reported() {
    let fixture = Fixture::new();
    fixture.app("Cap");
    fixture.download("Cap.dmg", 2);
    fs::remove_dir(fixture.home.join("Desktop")).unwrap();
    let found = scan(&fixture, &Versions::default());
    assert_eq!(found.files.len(), 1);
    assert!(found.unreadable.is_empty(), "absent is not unreadable");

    use std::os::unix::fs::PermissionsExt;
    let downloads = fixture.home.join("Downloads");
    fs::set_permissions(&downloads, fs::Permissions::from_mode(0o000)).unwrap();
    let denied = scan(&fixture, &Versions::default());
    fs::set_permissions(&downloads, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(denied.files.is_empty());
    assert_eq!(denied.unreadable, vec![downloads]);
}

#[test]
fn the_threshold_is_a_hundred_megabytes_of_private_bytes() {
    assert_eq!(INSTALLER_MIN_BYTES, 100_000_000);
    let fixture = Fixture::new();
    fixture.app("Big");
    fixture.app("Small");
    fixture.download("Big.dmg", 100);
    fixture.download("Small.dmg", 90);
    let found = scan_installers(&fixture.roots(), &Versions::default(), None);
    assert_eq!(names(&found), vec!["Big.dmg"]);
}

#[cfg(target_os = "macos")]
#[test]
fn an_installer_that_frees_nothing_is_not_offered() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let fixture = Fixture::new();
    fixture.app("Cap");
    fixture.app("Twin");
    let original = fixture.download("Twin-source.bin", 2);
    let twin = fixture.home.join("Downloads/Twin.dmg");
    let (from, to) = (
        CString::new(original.as_os_str().as_bytes()).unwrap(),
        CString::new(twin.as_os_str().as_bytes()).unwrap(),
    );
    // SAFETY: valid NUL-terminated paths for the duration of the call.
    if unsafe { libc::clonefile(from.as_ptr(), to.as_ptr(), 0) } != 0 {
        return; // not APFS: nothing to show
    }
    fixture.download("Cap.dmg", 2);
    assert_eq!(
        names(&scan(&fixture, &Versions::default())),
        vec!["Cap.dmg"]
    );
}

#[test]
fn a_cancelled_scan_returns_what_it_has_without_reading_on() {
    let fixture = Fixture::new();
    fixture.app("Cap");
    fixture.download("Cap.dmg", 2);
    let flag = AtomicBool::new(true);
    let found = scan_installers_above(
        MB as u64,
        &fixture.roots(),
        &Versions::default(),
        Some(&flag),
    );
    assert!(found.files.is_empty());
}

// ---------- move ----------

/// Moves into the fixture's `.Trash`, like the native route, and reports where.
struct FakeTrash {
    trash: PathBuf,
    moved: Mutex<Vec<PathBuf>>,
    fail: Option<TrashError>,
}

impl FakeTrash {
    fn new(fixture: &Fixture) -> Self {
        Self {
            trash: fixture.trash_dir(),
            moved: Mutex::default(),
            fail: None,
        }
    }
}

impl Trash for FakeTrash {
    fn move_to_trash(&self, path: &Path) -> Result<(), TrashError> {
        self.move_to_trash_reporting(path).map(|_| ())
    }

    fn move_to_trash_reporting(&self, path: &Path) -> Result<Option<PathBuf>, TrashError> {
        if let Some(error) = &self.fail {
            return Err(error.clone());
        }
        let target = self.trash.join(path.file_name().unwrap());
        fs::rename(path, &target).unwrap();
        self.moved.lock().unwrap().push(path.to_path_buf());
        Ok(Some(target))
    }
}

struct Run {
    fixture: Fixture,
    store: ProtectionStore,
    _store_dir: TempDir,
}

impl Run {
    fn new() -> Self {
        let store_dir = tempfile::tempdir().unwrap();
        Self {
            fixture: Fixture::new(),
            store: ProtectionStore::at(store_dir.path().join("clean-protection")),
            _store_dir: store_dir,
        }
    }

    fn plan(&self, files: &[&PathBuf]) -> Vec<PlannedMove> {
        files
            .iter()
            .map(|path| PlannedMove {
                path: (*path).clone(),
                fingerprint: crate::clean::fs_safe::fingerprint(path).unwrap(),
            })
            .collect()
    }

    fn run(
        &self,
        plan: &[PlannedMove],
        trash: &FakeTrash,
        cancel: Option<&AtomicBool>,
    ) -> super::move_run::MoveReport {
        let lease = self.store.lease().unwrap();
        let roots = self.fixture.roots().folders;
        let ctx = MoveContext {
            home: &self.fixture.home,
            roots: &roots,
            trash,
            cancel,
            progress: None,
            policy: &lease,
        };
        move_to_trash(plan, &ctx)
    }
}

#[test]
fn picked_files_move_with_their_trash_location_and_measured_bytes() {
    let run = Run::new();
    let a = run.fixture.download("A.dmg", 2);
    let b = run.fixture.download("B.dmg", 3);
    let trash = FakeTrash::new(&run.fixture);
    let report = run.run(&run.plan(&[&a, &b]), &trash, None);
    assert_eq!(report.moved_count(), 2);
    assert!(report.stopped.is_none());
    assert!(!a.exists() && !b.exists());
    let first = &report.results[0];
    assert_eq!(
        first.outcome,
        MoveOutcome::Moved {
            trashed_to: Some(run.fixture.trash_dir().join("A.dmg"))
        }
    );
    assert!(first.bytes_moved >= 2 * MB as u64 && !first.bytes_moved_is_upper_bound);
    assert!(first.fingerprint.is_some() && first.failure.is_none());
    assert_eq!(
        report.bytes_moved(),
        first.bytes_moved + report.results[1].bytes_moved
    );
}

#[test]
fn only_planned_paths_move() {
    let run = Run::new();
    let a = run.fixture.download("A.dmg", 2);
    let other = run.fixture.download("Other.dmg", 2);
    let trash = FakeTrash::new(&run.fixture);
    run.run(&run.plan(&[&a]), &trash, None);
    assert!(other.exists());
    assert_eq!(*trash.moved.lock().unwrap(), vec![a]);
}

#[test]
fn a_file_changed_since_the_scan_is_skipped_with_its_cause() {
    let run = Run::new();
    let a = run.fixture.download("A.dmg", 2);
    let plan = run.plan(&[&a]);
    fs::OpenOptions::new()
        .append(true)
        .open(&a)
        .unwrap()
        .write_all(b"more")
        .unwrap();
    let trash = FakeTrash::new(&run.fixture);
    let report = run.run(&plan, &trash, None);
    assert_eq!(
        report.results[0].outcome,
        MoveOutcome::Skipped(SkipReason::Changed)
    );
    assert_eq!(
        report.results[0].failure.as_ref().unwrap().cause,
        FailureCause::ChangedSinceScan
    );
    assert!(a.exists());
}

#[test]
fn a_gone_file_and_a_replacement_symlink_are_skipped() {
    let run = Run::new();
    let gone = run.fixture.download("Gone.dmg", 2);
    let swapped = run.fixture.download("Swapped.dmg", 2);
    let plan = run.plan(&[&gone, &swapped]);
    fs::remove_file(&gone).unwrap();
    fs::remove_file(&swapped).unwrap();
    std::os::unix::fs::symlink("/tmp", &swapped).unwrap();
    let trash = FakeTrash::new(&run.fixture);
    let report = run.run(&plan, &trash, None);
    assert_eq!(
        report.results[0].outcome,
        MoveOutcome::Skipped(SkipReason::Missing)
    );
    assert_eq!(
        report.results[0].failure.as_ref().unwrap().cause,
        FailureCause::AlreadyGone
    );
    assert_eq!(
        report.results[1].outcome,
        MoveOutcome::Skipped(SkipReason::Symlink)
    );
    assert!(trash.moved.lock().unwrap().is_empty());
}

#[test]
fn a_path_outside_the_scanned_folders_is_refused() {
    let run = Run::new();
    let outside = run.fixture.home.join("Documents");
    fs::create_dir_all(&outside).unwrap();
    let file = write(&outside.join("A.dmg"), 2);
    let trash = FakeTrash::new(&run.fixture);
    let report = run.run(&run.plan(&[&file]), &trash, None);
    assert_eq!(
        report.results[0].outcome,
        MoveOutcome::Skipped(SkipReason::OutsideRoots)
    );
    assert!(file.exists());
}

#[test]
fn a_saved_keep_rule_holds_the_file_and_is_not_a_failure() {
    let run = Run::new();
    let a = run.fixture.download("A.dmg", 2);
    {
        let mut lease = run.store.lease().unwrap();
        let revision = lease.snapshot().revision;
        lease.add(revision, &a, RuleScope::File).unwrap();
    }
    let trash = FakeTrash::new(&run.fixture);
    let report = run.run(&run.plan(&[&a]), &trash, None);
    assert!(matches!(
        report.results[0].outcome,
        MoveOutcome::Skipped(SkipReason::Kept { .. })
    ));
    assert!(report.results[0].failure.is_none());
    assert!(a.exists());
}

#[test]
fn cancelling_stops_the_run_and_the_rest_are_not_attempted() {
    let run = Run::new();
    let a = run.fixture.download("A.dmg", 2);
    let b = run.fixture.download("B.dmg", 2);
    let flag = AtomicBool::new(false);
    let trash = FakeTrash::new(&run.fixture);
    // The fake raises the run's flag itself after the first move.
    struct Raising<'a>(&'a AtomicBool, FakeTrash);
    impl Trash for Raising<'_> {
        fn move_to_trash(&self, path: &Path) -> Result<(), TrashError> {
            self.0.store(true, Ordering::Release);
            self.1.move_to_trash(path)
        }
    }
    let raising = Raising(&flag, trash);
    let lease = run.store.lease().unwrap();
    let roots = run.fixture.roots().folders;
    let ctx = MoveContext {
        home: &run.fixture.home,
        roots: &roots,
        trash: &raising,
        cancel: Some(&flag),
        progress: None,
        policy: &lease,
    };
    let report = move_to_trash(&run.plan(&[&a, &b]), &ctx);
    assert_eq!(report.stopped, Some(StopReason::Cancelled));
    assert!(matches!(
        report.results[0].outcome,
        MoveOutcome::Moved { .. }
    ));
    assert_eq!(report.results[1].outcome, MoveOutcome::NotAttempted);
    assert!(b.exists());
}

#[test]
fn a_refused_move_is_a_failure_with_its_cause_and_the_run_goes_on() {
    let run = Run::new();
    let a = run.fixture.download("A.dmg", 2);
    let mut trash = FakeTrash::new(&run.fixture);
    trash.fail = Some(TrashError::Os {
        code: libc::EBUSY,
        detail: "busy".into(),
    });
    let report = run.run(&run.plan(&[&a]), &trash, None);
    assert!(matches!(
        report.results[0].outcome,
        MoveOutcome::Failed {
            source_present: true,
            ..
        }
    ));
    assert_eq!(
        report.results[0].failure.as_ref().unwrap().cause,
        FailureCause::InUse
    );
    assert_eq!(report.results[0].bytes_moved, 0);
    assert!(a.exists());
}

// ---------- undo ----------

fn moved_item(run: &Run, path: &PathBuf) -> UndoItem {
    let trash = FakeTrash::new(&run.fixture);
    let report = run.run(&run.plan(&[path]), &trash, None);
    let result = &report.results[0];
    let MoveOutcome::Moved { trashed_to } = &result.outcome else {
        panic!("did not move: {:?}", result.outcome);
    };
    UndoItem {
        original: path.clone(),
        trashed: trashed_to.clone().unwrap(),
        fingerprint: result.fingerprint.clone().unwrap(),
    }
}

#[test]
fn undo_puts_the_file_back_where_it_was() {
    let run = Run::new();
    let a = run.fixture.download("A.dmg", 2);
    let before = fs::read(&a).unwrap();
    let item = moved_item(&run, &a);
    assert!(!a.exists());
    assert_eq!(
        undo_moves(std::slice::from_ref(&item)),
        vec![UndoOutcome::Restored]
    );
    assert_eq!(fs::read(&a).unwrap(), before);
    assert!(!item.trashed.exists());
}

#[test]
fn undo_never_replaces_something_that_exists_now() {
    let run = Run::new();
    let a = run.fixture.download("A.dmg", 2);
    let item = moved_item(&run, &a);
    fs::write(&a, b"new").unwrap();
    assert_eq!(
        undo_moves(std::slice::from_ref(&item)),
        vec![UndoOutcome::OriginalExists]
    );
    assert_eq!(fs::read(&a).unwrap(), b"new");
    assert!(item.trashed.exists(), "still in the Trash");
}

#[test]
fn undo_answers_for_each_item_and_names_why_it_cannot() {
    let run = Run::new();
    let emptied = run.fixture.download("Emptied.dmg", 2);
    let replaced = run.fixture.download("Replaced.dmg", 2);
    let nofolder = run.fixture.download("NoFolder.dmg", 2);
    let fine = run.fixture.download("Fine.dmg", 2);
    let items: Vec<UndoItem> = [&emptied, &replaced, &nofolder, &fine]
        .into_iter()
        .map(|p| moved_item(&run, p))
        .collect();
    fs::remove_file(&items[0].trashed).unwrap();
    fs::write(&items[1].trashed, b"something else").unwrap();
    let mut elsewhere = items[2].clone();
    elsewhere.original = run.fixture.home.join("Gone/NoFolder.dmg");
    let mut unknown = items[3].clone();
    unknown.trashed = PathBuf::from("/tmp/not-a-trash/Fine.dmg");
    assert_eq!(
        undo_moves(&[
            items[0].clone(),
            items[1].clone(),
            elsewhere,
            unknown,
            items[3].clone()
        ]),
        vec![
            UndoOutcome::NotInTrash,
            UndoOutcome::Replaced,
            UndoOutcome::FolderMissing,
            UndoOutcome::UnknownLocation,
            UndoOutcome::Restored,
        ]
    );
}
