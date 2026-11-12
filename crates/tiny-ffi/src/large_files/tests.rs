//! Adapter tests. They use only temp dirs they create, a fake `Trash` that
//! renames inside the fixture's own `.Trash`, a fake version table and a
//! temp saved-rules store: never the real home, Applications, Finder or
//! Application Support.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Duration;

use tiny_core::clean::finder_trash::TrashError;
use tiny_core::clean::protection::RuleScope;
use tiny_core::clean::protection_store::ProtectionStore;

use super::*;
use crate::clean::{FfiFailureCause, FfiSkipReason, FfiStopReason};
use crate::FfiProgress;

const MB: usize = 1 << 20;
/// `EBUSY` on macOS.
const EBUSY: i32 = 16;

/// A canonical temp directory removed on drop (macOS's `/var` is a symlink,
/// which the move checks refuse on the way to a file).
struct TempDir(PathBuf);

impl TempDir {
    fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "tiny-ffi-large-files-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        fs::create_dir_all(&path).unwrap();
        Self(path.canonicalize().unwrap())
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct Listener(Mutex<Vec<FfiProgress>>);

impl ProgressListener for Listener {
    fn on_progress(&self, progress: FfiProgress) {
        self.0.lock().unwrap().push(progress);
    }
}

struct NoVersions;

/// No app is running.
struct NoApps;

impl tiny_core::clean::process::AppProbe for NoApps {
    fn probe(&self, _: &str) -> Result<bool, String> {
        Ok(false)
    }
}

impl AppVersions for NoVersions {
    fn version(&self, _: &Path) -> Option<String> {
        None
    }
}

struct FakeTrash {
    trash: PathBuf,
    fail: Option<TrashError>,
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
        Ok(Some(target))
    }
}

struct World {
    _dir: TempDir,
    home: PathBuf,
    session: Arc<TinySession>,
    rules: PathBuf,
}

impl World {
    fn new() -> Self {
        let dir = TempDir::new();
        let base = dir.0.clone();
        let home = base.join("home");
        for folder in ["Downloads", "Desktop", "Applications", ".Trash"] {
            fs::create_dir_all(home.join(folder)).unwrap();
        }
        fs::create_dir_all(base.join("apps")).unwrap();
        let rules = base.join("rules/clean-protection");
        let session = TinySession::new();
        session.inject_clean_inputs(&home, &home, rules.clone());
        Self {
            _dir: dir,
            home,
            session,
            rules,
        }
    }

    fn roots(&self) -> InstallerRoots {
        InstallerRoots {
            folders: vec![self.home.join("Downloads"), self.home.join("Desktop")],
            applications: vec![self.home.join("Applications")],
        }
    }

    /// An installed app and a 2 MB installer for it.
    fn installer(&self, app: &str, file: &str) -> PathBuf {
        fs::create_dir_all(self.home.join("Applications").join(format!("{app}.app"))).unwrap();
        let path = self.home.join("Downloads").join(file);
        let block: Vec<u8> = (0..MB).map(|i| (i % 251) as u8).collect();
        let mut out = fs::File::create(&path).unwrap();
        out.write_all(&block).unwrap();
        out.write_all(&block).unwrap();
        path
    }

    fn scan(&self) -> FfiLargeFilesScan {
        self.session
            .scan_large_files(
                &self.roots(),
                MB as u64,
                &NoVersions,
                None,
                &CancellationToken::default(),
                &Listener::default(),
            )
            .unwrap()
    }

    fn trash(&self) -> FakeTrash {
        FakeTrash {
            trash: self.home.join(".Trash"),
            fail: None,
        }
    }

    fn run(
        &self,
        preview_id: &str,
        trash: &FakeTrash,
    ) -> Result<FfiLargeFilesMoveReport, FfiError> {
        self.session.move_large_files(
            preview_id,
            trash,
            &CancellationToken::default(),
            &Listener::default(),
        )
    }

    fn keep(&self, path: &Path) {
        let store = ProtectionStore::at(self.rules.clone());
        let mut lease = store.lease().unwrap();
        let revision = lease.snapshot().revision;
        lease.add(revision, path, RuleScope::File).unwrap();
    }
}

fn ids(scan: &FfiLargeFilesScan) -> Vec<String> {
    scan.files.iter().map(|f| f.id.clone()).collect()
}

fn invalid(result: Result<impl std::fmt::Debug, FfiError>, needle: &str) {
    match result {
        Err(FfiError::PreviewInvalid { detail }) => assert!(detail.contains(needle), "{detail}"),
        other => panic!("expected a refused preview ({needle}), got {other:?}"),
    }
}

// ---------- scan ----------

#[test]
fn a_scan_returns_records_for_installers_whose_app_is_installed() {
    let world = World::new();
    let cap = world.installer("Cap", "Cap-1.4.2.dmg");
    world.installer("Other", "Unrelated-9.dmg");
    fs::remove_dir(world.home.join("Applications/Other.app")).unwrap();
    let scan = world.scan();
    assert_eq!(scan.files.len(), 1);
    let file = &scan.files[0];
    assert_eq!(file.path, cap.to_string_lossy());
    assert_eq!(file.name, "Cap installer");
    assert_eq!(file.group, "installers");
    assert_eq!(file.reason, "Cap is installed in Applications");
    assert!(file.frees_bytes >= 2 * MB as u64 && file.frees_is_exact);
    assert_eq!(
        file.matched_app_path.as_deref(),
        Some(world.home.join("Applications/Cap.app").to_str().unwrap())
    );
    assert!(scan.unreadable_folders.is_empty());
}

#[test]
fn a_scan_holds_the_gate_and_a_busy_session_refuses_it() {
    let world = World::new();
    let _held = world.session.begin().unwrap();
    let busy = world.session.scan_large_files(
        &world.roots(),
        MB as u64,
        &NoVersions,
        None,
        &CancellationToken::default(),
        &Listener::default(),
    );
    assert!(matches!(busy, Err(FfiError::Busy)));
}

#[test]
fn a_cancelled_scan_changes_nothing() {
    let world = World::new();
    world.installer("Cap", "Cap.dmg");
    let first = world.scan();
    let token = CancellationToken::default();
    token.cancel();
    let cancelled = world.session.scan_large_files(
        &world.roots(),
        MB as u64,
        &NoVersions,
        None,
        &token,
        &Listener::default(),
    );
    assert!(matches!(cancelled, Err(FfiError::Cancelled)));
    // The first scan's IDs still work.
    assert!(world.session.preview_large_files(&ids(&first)).is_ok());
}

// ---------- preview ----------

#[test]
fn a_preview_is_built_from_the_current_scans_ids_only() {
    let world = World::new();
    let preview = world.session.preview_large_files(&["l1-0".into()]);
    invalid(preview, "no current scan");
    world.installer("Cap", "Cap.dmg");
    let scan = world.scan();
    invalid(
        world.session.preview_large_files(&["forged".into()]),
        "unknown file",
    );
    assert!(matches!(
        world.session.preview_large_files(&[]),
        Err(FfiError::InvalidInput { .. })
    ));
    let preview = world.session.preview_large_files(&ids(&scan)).unwrap();
    assert_eq!(preview.items, scan.files);
    assert_eq!(preview.frees_bytes, scan.files[0].frees_bytes);
    assert!(preview.frees_is_exact && preview.held_ids.is_empty());
    assert_eq!(preview.expires_in_seconds, PREVIEW_TTL.as_secs());
}

#[test]
fn a_file_a_saved_rule_holds_is_listed_as_held_and_left_out() {
    let world = World::new();
    let kept = world.installer("Cap", "Cap.dmg");
    world.installer("Zed", "Zed.dmg");
    world.keep(&kept);
    let scan = world.scan();
    let preview = world.session.preview_large_files(&ids(&scan)).unwrap();
    assert_eq!(preview.items.len(), 1);
    assert_eq!(preview.held_ids.len(), 1);
    assert!(preview.items[0].name.starts_with("Zed"));
}

// ---------- move ----------

#[test]
fn only_listed_ids_move_and_the_report_says_where_each_went() {
    let world = World::new();
    let cap = world.installer("Cap", "Cap.dmg");
    let zed = world.installer("Zed", "Zed.dmg");
    let scan = world.scan();
    let picked: Vec<String> = scan
        .files
        .iter()
        .filter(|f| f.path == cap.to_string_lossy())
        .map(|f| f.id.clone())
        .collect();
    let preview = world.session.preview_large_files(&picked).unwrap();
    let report = world.run(&preview.preview_id, &world.trash()).unwrap();
    assert_eq!(
        (
            report.moved_count,
            report.failed_count,
            report.skipped_count
        ),
        (1, 0, 0)
    );
    assert!(!cap.exists() && zed.exists());
    let result = &report.results[0];
    assert_eq!(result.outcome, FfiItemOutcome::MovedToTrash);
    assert_eq!(
        result.trashed_path.as_deref(),
        Some(world.home.join(".Trash/Cap.dmg").to_str().unwrap())
    );
    assert!(result.bytes_moved >= 2 * MB as u64);
    assert_eq!(report.bytes_moved_to_trash, result.bytes_moved);
    assert!(report.move_id.is_some() && report.stopped.is_none());
    // What moved is gone from the list.
    invalid(world.session.preview_large_files(&picked), "unknown file");
}

#[test]
fn a_preview_runs_once() {
    let world = World::new();
    world.installer("Cap", "Cap.dmg");
    let scan = world.scan();
    let preview = world.session.preview_large_files(&ids(&scan)).unwrap();
    world.run(&preview.preview_id, &world.trash()).unwrap();
    invalid(
        world.run(&preview.preview_id, &world.trash()),
        "already used",
    );
    invalid(world.run("lp999", &world.trash()), "unknown or replaced");
}

#[test]
fn an_expired_preview_is_refused_and_a_newer_preview_replaces_the_older() {
    let world = World::new();
    let cap = world.installer("Cap", "Cap.dmg");
    let scan = world.scan();
    world.session.large_files_state().ttl = Some(Duration::ZERO);
    let expired = world.session.preview_large_files(&ids(&scan)).unwrap();
    invalid(world.run(&expired.preview_id, &world.trash()), "expired");
    assert!(cap.exists());
    world.session.large_files_state().ttl = None;
    let first = world.session.preview_large_files(&ids(&scan)).unwrap();
    let second = world.session.preview_large_files(&ids(&scan)).unwrap();
    invalid(
        world.run(&first.preview_id, &world.trash()),
        "unknown or replaced",
    );
    assert!(world.run(&second.preview_id, &world.trash()).is_ok());
}

#[test]
fn a_rescan_after_the_preview_makes_it_stale() {
    let world = World::new();
    let cap = world.installer("Cap", "Cap.dmg");
    let scan = world.scan();
    let preview = world.session.preview_large_files(&ids(&scan)).unwrap();
    world.scan();
    invalid(
        world.run(&preview.preview_id, &world.trash()),
        "unknown or replaced",
    );
    assert!(cap.exists());
}

#[test]
fn a_busy_session_refuses_the_move_and_leaves_the_preview_usable() {
    let world = World::new();
    let cap = world.installer("Cap", "Cap.dmg");
    let scan = world.scan();
    let preview = world.session.preview_large_files(&ids(&scan)).unwrap();
    {
        let _held = world.session.begin().unwrap();
        assert!(matches!(
            world.run(&preview.preview_id, &world.trash()),
            Err(FfiError::Busy)
        ));
    }
    assert!(cap.exists());
    assert!(world.run(&preview.preview_id, &world.trash()).is_ok());
}

#[test]
fn rules_saved_after_the_preview_stop_the_move_unspent() {
    let world = World::new();
    let cap = world.installer("Cap", "Cap.dmg");
    let scan = world.scan();
    let preview = world.session.preview_large_files(&ids(&scan)).unwrap();
    world.keep(&cap);
    assert!(matches!(
        world.run(&preview.preview_id, &world.trash()),
        Err(FfiError::ProtectionChanged)
    ));
    assert!(cap.exists());
    invalid(
        world.run(&preview.preview_id, &world.trash()),
        "unknown or replaced",
    );
}

#[test]
fn a_file_changed_after_the_scan_is_skipped_by_cause() {
    let world = World::new();
    let cap = world.installer("Cap", "Cap.dmg");
    let scan = world.scan();
    let preview = world.session.preview_large_files(&ids(&scan)).unwrap();
    fs::OpenOptions::new()
        .append(true)
        .open(&cap)
        .unwrap()
        .write_all(b"x")
        .unwrap();
    let report = world.run(&preview.preview_id, &world.trash()).unwrap();
    assert_eq!(report.skipped_count, 1);
    assert!(matches!(
        report.results[0].outcome,
        FfiItemOutcome::Skipped {
            reason: FfiSkipReason::Changed,
            ..
        }
    ));
    assert_eq!(
        report.results[0].cause,
        Some(FfiFailureCause::ChangedSinceScan)
    );
    assert!(report.move_id.is_none() && cap.exists());
}

#[test]
fn a_refused_move_is_reported_by_cause_and_cancelling_stops_the_rest() {
    let world = World::new();
    let cap = world.installer("Cap", "Cap.dmg");
    let scan = world.scan();
    let preview = world.session.preview_large_files(&ids(&scan)).unwrap();
    let mut trash = world.trash();
    trash.fail = Some(TrashError::Os {
        code: EBUSY,
        detail: "busy".into(),
    });
    let report = world.run(&preview.preview_id, &trash).unwrap();
    assert_eq!(report.failed_count, 1);
    assert_eq!(report.results[0].cause, Some(FfiFailureCause::InUse));
    assert!(cap.exists() && report.move_id.is_none());

    let scan = world.scan();
    let preview = world.session.preview_large_files(&ids(&scan)).unwrap();
    let token = CancellationToken::default();
    token.cancel();
    let report = world
        .session
        .move_large_files(
            &preview.preview_id,
            &world.trash(),
            &token,
            &Listener::default(),
        )
        .unwrap();
    assert_eq!(report.stopped, Some(FfiStopReason::Cancelled));
    assert_eq!(report.not_attempted_count, 1);
    assert!(cap.exists());
}

// ---------- undo ----------

fn moved_cap(world: &World) -> (PathBuf, FfiLargeFilesMoveReport, FfiLargeFilesScan) {
    let cap = world.installer("Cap", "Cap.dmg");
    let scan = world.scan();
    let preview = world.session.preview_large_files(&ids(&scan)).unwrap();
    let report = world.run(&preview.preview_id, &world.trash()).unwrap();
    (cap, report, scan)
}

#[test]
fn undo_puts_the_file_back_and_back_into_the_list() {
    let world = World::new();
    let (cap, report, scan) = moved_cap(&world);
    assert!(!cap.exists());
    let undo = world
        .session
        .large_files_undo(report.move_id.clone().unwrap())
        .unwrap();
    assert_eq!(undo.restored_count, 1);
    assert_eq!(undo.results[0].outcome, FfiUndoOutcome::Restored);
    assert!(cap.exists());
    // Same scan, same ID: it can be picked again without a rescan.
    assert!(world.session.preview_large_files(&ids(&scan)).is_ok());
    // Used up.
    invalid(
        world.session.large_files_undo(report.move_id.unwrap()),
        "nothing to undo",
    );
}

#[test]
fn undo_refuses_to_replace_and_can_be_tried_again() {
    let world = World::new();
    let (cap, report, _) = moved_cap(&world);
    let move_id = report.move_id.unwrap();
    fs::write(&cap, b"new").unwrap();
    let first = world.session.large_files_undo(move_id.clone()).unwrap();
    assert_eq!(
        (first.restored_count, &first.results[0].outcome),
        (0, &FfiUndoOutcome::OriginalExists)
    );
    assert_eq!(fs::read(&cap).unwrap(), b"new");
    fs::remove_file(&cap).unwrap();
    let second = world.session.large_files_undo(move_id).unwrap();
    assert_eq!(second.restored_count, 1);
}

#[test]
fn undo_is_refused_while_busy_and_for_an_unknown_move() {
    let world = World::new();
    let (_, report, _) = moved_cap(&world);
    invalid(
        world.session.large_files_undo("lm999".into()),
        "nothing to undo",
    );
    let _held = world.session.begin().unwrap();
    assert!(matches!(
        world.session.large_files_undo(report.move_id.unwrap()),
        Err(FfiError::Busy)
    ));
}

#[test]
fn a_later_move_replaces_what_undo_takes() {
    let world = World::new();
    let (_, first, _) = moved_cap(&world);
    let zed = world.installer("Zed", "Zed.dmg");
    let scan = world.scan();
    let preview = world.session.preview_large_files(&ids(&scan)).unwrap();
    let second = world.run(&preview.preview_id, &world.trash()).unwrap();
    assert!(!zed.exists());
    invalid(
        world.session.large_files_undo(first.move_id.unwrap()),
        "nothing to undo",
    );
    assert_eq!(
        world
            .session
            .large_files_undo(second.move_id.unwrap())
            .unwrap()
            .restored_count,
        1
    );
}

#[test]
fn a_run_that_moved_nothing_keeps_the_previous_moves_undo() {
    let world = World::new();
    let (cap, first, _) = moved_cap(&world);
    let zed = world.installer("Zed", "Zed.dmg");
    let scan = world.scan();
    let preview = world.session.preview_large_files(&ids(&scan)).unwrap();
    let mut trash = world.trash();
    trash.fail = Some(TrashError::Os {
        code: EBUSY,
        detail: "busy".into(),
    });
    let second = world.run(&preview.preview_id, &trash).unwrap();
    assert!(second.move_id.is_none() && zed.exists());
    let undo = world
        .session
        .large_files_undo(first.move_id.unwrap())
        .unwrap();
    assert_eq!(undo.restored_count, 1);
    assert!(cap.exists());
}

// ---------- groups 3 and 4: the home walk ----------

mod home_walk {
    use super::*;
    use tiny_core::clean::large_files::last_used::NoLastUsed;

    fn old_file(world: &World, relative: &str, days: u64) -> PathBuf {
        let path = world.home.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, vec![7u8; 2 * MB]).unwrap();
        let file = fs::File::options().write(true).open(&path).unwrap();
        file.set_modified(std::time::SystemTime::now() - Duration::from_secs(86_400 * days))
            .unwrap();
        path
    }

    fn scan(world: &World) -> FfiLargeFilesScan {
        let limits = Limits {
            copy_min_bytes: MB as u64,
            only_copy_min_bytes: MB as u64,
            other_min_bytes: MB as u64,
            other_min_age_days: 90,
        };
        world
            .session
            .scan_large_files(
                &world.roots(),
                MB as u64,
                &NoVersions,
                Some(HomeSettings {
                    home: &world.home,
                    limits,
                    last_used: &NoLastUsed,
                    probe: &NoApps,
                }),
                &CancellationToken::default(),
                &Listener::default(),
            )
            .unwrap()
    }

    #[test]
    fn a_scan_returns_the_three_groups_and_an_installer_is_listed_once() {
        let world = World::new();
        let cap = world.installer("Cap", "Cap-1.4.2.dmg");
        let orphan = old_file(&world, "Downloads/Gone-2.0.dmg", 120);
        let video = old_file(&world, "Documents/stream.mp4", 3);
        let archive = old_file(&world, "Documents/backup.zip", 200);
        let scan = scan(&world);
        let group_of = |path: &Path| {
            let hits: Vec<_> = scan
                .files
                .iter()
                .filter(|f| f.path == path.to_string_lossy())
                .collect();
            assert_eq!(hits.len(), 1, "{path:?} listed {} times", hits.len());
            hits[0].group.clone()
        };
        assert_eq!(group_of(&cap), "installers");
        assert_eq!(group_of(&orphan), "other_large");
        assert_eq!(group_of(&video), "only_copy");
        assert_eq!(group_of(&archive), "other_large");
        let video = scan.files.iter().find(|f| f.group == "only_copy").unwrap();
        assert_eq!(video.reason, "Video · changed 3 days ago");
        assert!(video.matched_app_path.is_none());
    }

    #[test]
    fn a_group_4_file_moves_and_comes_back_through_the_same_consent_path() {
        let world = World::new();
        let archive = old_file(&world, "Documents/deep/backup.zip", 200);
        let scan = scan(&world);
        assert_eq!(scan.files.len(), 1);
        let preview = world.session.preview_large_files(&ids(&scan)).unwrap();
        let report = world.run(&preview.preview_id, &world.trash()).unwrap();
        assert_eq!(report.moved_count, 1, "{report:?}");
        assert!(!archive.exists());
        let undo = world
            .session
            .large_files_undo(report.move_id.unwrap())
            .unwrap();
        assert_eq!(undo.restored_count, 1);
        assert!(archive.exists());
    }

    #[test]
    fn group_5_rows_are_listed_with_their_facts_and_the_preview_refuses_them() {
        let world = World::new();
        let vm = world
            .home
            .join("Library/Containers/com.utmapp.UTM/Data/Documents/Win.utm");
        fs::create_dir_all(vm.join("Data")).unwrap();
        fs::write(
            vm.join("config.plist"),
            "<plist><dict><key>Information</key><dict><key>Name</key><string>Win</string></dict></dict></plist>",
        )
        .unwrap();
        fs::write(vm.join("screenshot.png"), b"png").unwrap();
        fs::write(vm.join("Data/disk.qcow2"), vec![7u8; 2 * MB]).unwrap();
        let archive = old_file(&world, "Documents/backup.zip", 200);
        let scan = scan(&world);
        let row = scan.files.iter().find(|f| f.group == "elsewhere").unwrap();
        assert_eq!(row.path, vm.to_string_lossy());
        assert_eq!(row.name, "Win · UTM virtual machine");
        assert_eq!(row.kind.as_deref(), Some("Virtual machine"));
        assert_eq!(row.app_name.as_deref(), Some("UTM"));
        assert_eq!(row.app_bundle_id.as_deref(), Some("com.utmapp.UTM"));
        assert!(!row.app_running);
        assert_eq!(
            row.preview_path.as_deref(),
            Some(vm.join("screenshot.png").to_str().unwrap())
        );
        assert!(row.shared_bytes.is_none());
        assert!(row.last_changed.is_some());
        assert!(row.frees_bytes >= 2 * MB as u64);
        // Group 5 is never movable: the preview refuses it, alone or among others.
        let other = scan
            .files
            .iter()
            .find(|f| f.path == archive.to_string_lossy())
            .unwrap();
        for asked in [vec![row.id.clone()], vec![other.id.clone(), row.id.clone()]] {
            assert!(matches!(
                world.session.preview_large_files(&asked),
                Err(FfiError::InvalidInput { .. })
            ));
        }
        // Nothing was spent: the other file still previews.
        assert!(world
            .session
            .preview_large_files(std::slice::from_ref(&other.id))
            .is_ok());
        assert!(vm.exists());
    }

    fn el(id: &[u8], body: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.push(0x01);
        out.extend_from_slice(&(body.len() as u64).to_be_bytes()[1..]);
        out.extend_from_slice(body);
        out
    }

    /// A readable MKV (1920x1080, 60 s, no audio) padded to `megabytes`.
    fn mkv(world: &World, relative: &str, megabytes: usize) -> PathBuf {
        let mut info = el(&[0x2A, 0xD7, 0xB1], &1_000_000u64.to_be_bytes());
        info.extend(el(&[0x44, 0x89], &60_000f64.to_be_bytes()));
        let mut entry = el(&[0xD7], &1u64.to_be_bytes());
        entry.extend(el(&[0x73, 0xC5], &1u64.to_be_bytes()));
        entry.extend(el(&[0x83], &1u64.to_be_bytes()));
        let mut picture = el(&[0xB0], &1920u64.to_be_bytes());
        picture.extend(el(&[0xBA], &1080u64.to_be_bytes()));
        entry.extend(el(&[0xE0], &picture));
        let mut segment = el(&[0x15, 0x49, 0xA9, 0x66], &info);
        segment.extend(el(&[0x16, 0x54, 0xAE, 0x6B], &el(&[0xAE], &entry)));
        segment.extend(el(&[0xEC], &vec![0u8; megabytes * MB]));
        let mut file = el(&[0x1A, 0x45, 0xDF, 0xA3], &el(&[0x42, 0x82], b"matroska"));
        file.extend(el(&[0x18, 0x53, 0x80, 0x67], &segment));
        let path = world.home.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, file).unwrap();
        path
    }

    #[test]
    fn a_copy_row_carries_its_facts_and_never_moves_with_the_file_that_stays() {
        let world = World::new();
        let stays = mkv(&world, "Videos/Raw/talk.mkv", 3);
        let copy = mkv(&world, "Videos/Backup/talk.mkv", 2);
        let scan = scan(&world);
        let row = scan.files.iter().find(|f| f.group == "copies").unwrap();
        assert_eq!(row.path, copy.to_string_lossy());
        assert_eq!(row.keeper_path.as_deref(), stays.to_str());
        assert_eq!(row.keep_rule.as_deref(), Some("larger"));
        assert_eq!(
            (row.duration_ms, row.keeper_duration_ms),
            (Some(60_000), Some(60_000))
        );
        assert_eq!(
            (row.width, row.height, row.audio_tracks),
            (Some(1920), Some(1080), Some(0))
        );
        let keeper = scan
            .files
            .iter()
            .find(|f| f.path == stays.to_string_lossy())
            .unwrap();
        assert_eq!(keeper.group, "only_copy");
        // Both in one move would lose the recording: refused, nothing spent.
        assert!(matches!(
            world
                .session
                .preview_large_files(&[row.id.clone(), keeper.id.clone()]),
            Err(FfiError::InvalidInput { .. })
        ));
        let preview = world
            .session
            .preview_large_files(std::slice::from_ref(&row.id))
            .unwrap();
        // The file that stays disappears before the move: the copy is kept.
        fs::remove_file(&stays).unwrap();
        let report = world.run(&preview.preview_id, &world.trash()).unwrap();
        assert_eq!(
            (report.moved_count, report.skipped_count),
            (0, 1),
            "{report:?}"
        );
        assert!(copy.exists());
    }
}
