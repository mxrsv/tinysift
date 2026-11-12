//! Groups 3 and 4 on temp-dir fixtures with small limits (1 MB instead of
//! 500 MB). Nothing reads the real home.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use tempfile::TempDir;

use super::discovery::flags_are_dataless;
use super::last_used::{parse_mdls_date, LastUsed, NoLastUsed};
use super::*;

const MB: usize = 1 << 20;
const DAY: Duration = Duration::from_secs(86_400);

struct Home {
    _dir: TempDir,
    root: PathBuf,
    now: SystemTime,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let root = base.join("home");
        fs::create_dir_all(root.join("Documents")).unwrap();
        Self {
            _dir: dir,
            root,
            now: SystemTime::now(),
        }
    }

    /// A file of `megabytes` MB changed `days` ago, at `relative` in the home.
    fn file(&self, relative: &str, megabytes: usize, days: u64) -> PathBuf {
        let path = self.root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let block: Vec<u8> = (0..MB).map(|i| (i % 251) as u8).collect();
        let mut out = fs::File::create(&path).unwrap();
        for _ in 0..megabytes {
            out.write_all(&block).unwrap();
        }
        out.set_modified(self.now - DAY * days as u32).unwrap();
        path
    }

    fn scan_with(&self, used: &dyn LastUsed, claimed: &HashSet<PathBuf>) -> HomeScan {
        let walk = HomeWalk {
            home: &self.root,
            limits: Limits {
                copy_min_bytes: MB as u64,
                only_copy_min_bytes: MB as u64,
                other_min_bytes: MB as u64,
                other_min_age_days: 90,
            },
            last_used: used,
            now: self.now,
            claimed,
        };
        scan_home(&walk, None, None)
    }

    fn scan(&self) -> HomeScan {
        self.scan_with(&NoLastUsed, &HashSet::new())
    }

    fn names(&self, scan: &HomeScan) -> Vec<String> {
        scan.files.iter().map(|f| f.name.clone()).collect()
    }
}

#[derive(Default)]
struct Used(Mutex<HashMap<PathBuf, SystemTime>>);

impl LastUsed for Used {
    fn last_used(&self, path: &Path) -> Option<SystemTime> {
        self.0.lock().unwrap().get(path).copied()
    }
}

#[test]
fn a_video_is_only_copy_whatever_its_age_and_says_when_it_changed() {
    let home = Home::new();
    home.file("Documents/stream.mp4", 2, 31);
    home.file("Documents/today.MOV", 2, 0);
    let scan = home.scan();
    let by_name: HashMap<_, _> = scan.files.iter().map(|f| (f.name.as_str(), f)).collect();
    assert_eq!(by_name["stream"].group, Group::OnlyCopy);
    assert_eq!(by_name["stream"].reason, "Video · changed 31 days ago");
    assert_eq!(by_name["today"].group, Group::OnlyCopy);
    assert_eq!(by_name["today"].reason, "Video · changed today");
    assert!(by_name["stream"].matched_app.is_none());
}

#[test]
fn every_video_extension_counts_and_a_small_video_does_not() {
    let home = Home::new();
    for ext in VIDEO_EXTENSIONS {
        home.file(&format!("Documents/clip.{ext}"), 2, 1);
    }
    home.file("Documents/small/clip.mp4", 0, 1);
    let scan = home.scan();
    assert_eq!(scan.files.len(), VIDEO_EXTENSIONS.len());
    assert!(scan.files.iter().all(|f| f.group == Group::OnlyCopy));
}

#[test]
fn other_large_files_need_the_size_and_ninety_days() {
    let home = Home::new();
    home.file("Documents/old.zip", 2, 91);
    home.file("Documents/edge.zip", 2, 90);
    home.file("Documents/young.zip", 2, 89);
    home.file("Documents/tiny.zip", 0, 400);
    let scan = home.scan();
    let mut names = home.names(&scan);
    names.sort();
    assert_eq!(names, ["edge", "old"]);
    let old = scan.files.iter().find(|f| f.name == "old").unwrap();
    assert_eq!(old.group, Group::OtherLarge);
    assert_eq!(old.reason, "Archive · not changed for 91 days");
}

#[test]
fn a_kind_comes_from_the_extension_or_says_it_is_a_file() {
    let home = Home::new();
    home.file("Documents/a.dmg", 2, 100);
    home.file("Documents/b.xyz", 2, 100);
    home.file("Documents/c", 2, 100);
    let reasons: HashMap<_, _> = home
        .scan()
        .files
        .into_iter()
        .map(|f| (f.name, f.reason))
        .collect();
    assert_eq!(reasons["a"], "Disk image · not changed for 100 days");
    assert_eq!(reasons["b"], "XYZ file · not changed for 100 days");
    assert_eq!(reasons["c"], "File · not changed for 100 days");
}

#[test]
fn a_newer_spotlight_date_keeps_a_file_out_and_an_older_one_is_ignored() {
    let home = Home::new();
    let used_lately = home.file("Documents/used.zip", 2, 200);
    let used_long_ago = home.file("Documents/stale.zip", 2, 200);
    let used_old_enough = home.file("Documents/opened.zip", 2, 200);
    let used = Used::default();
    {
        let mut table = used.0.lock().unwrap();
        table.insert(used_lately, home.now - DAY * 3);
        table.insert(used_long_ago, home.now - DAY * 400);
        table.insert(used_old_enough, home.now - DAY * 120);
    }
    let scan = home.scan_with(&used, &HashSet::new());
    let reasons: HashMap<_, _> = scan.files.iter().map(|f| (f.name.as_str(), f)).collect();
    assert!(!reasons.contains_key("used"));
    assert_eq!(
        reasons["stale"].reason,
        "Archive · not changed for 200 days"
    );
    assert_eq!(reasons["opened"].reason, "Archive · not used for 120 days");
}

#[test]
fn spotlight_is_never_asked_about_a_recent_file() {
    struct Panics;
    impl LastUsed for Panics {
        fn last_used(&self, _: &Path) -> Option<SystemTime> {
            panic!("asked about a file that is too recent or too small");
        }
    }
    let home = Home::new();
    home.file("Documents/young.zip", 2, 10);
    home.file("Documents/video.mp4", 2, 500);
    assert_eq!(home.scan_with(&Panics, &HashSet::new()).files.len(), 1);
}

#[test]
fn rows_sort_by_what_they_free_across_both_groups() {
    let home = Home::new();
    home.file("Documents/two.zip", 2, 100);
    home.file("Documents/four.mp4", 4, 5);
    home.file("Documents/three.zip", 3, 100);
    assert_eq!(home.names(&home.scan()), ["four", "three", "two"]);
}

#[test]
fn a_file_another_group_claimed_is_left_out() {
    let home = Home::new();
    let claimed = home.file("Downloads/Cap.dmg", 2, 100);
    home.file("Downloads/Orphan.dmg", 2, 100);
    let scan = home.scan_with(&NoLastUsed, &HashSet::from([claimed]));
    assert_eq!(home.names(&scan), ["Orphan"]);
    assert_eq!(scan.files[0].group, Group::OtherLarge);
}

// ---------- NL1: never listed ----------

#[test]
fn an_app_bundle_and_a_package_library_are_never_listed() {
    let home = Home::new();
    home.file("Applications/Big.app/Contents/Resources/data.bin", 2, 400);
    home.file(
        "Pictures/Photos Library.photoslibrary/originals/a.mov",
        2,
        400,
    );
    home.file("Music/Music Library.musiclibrary/a.m4a", 2, 400);
    home.file("Movies/Movie.fcpbundle/media/a.mov", 2, 400);
    home.file("Movies/iMovie Library.imovielibrary/a.mov", 2, 400);
    home.file("Music/Music/Media/song.m4a", 2, 400);
    home.file("Movies/TV/Media/show.mp4", 2, 400);
    home.file("Documents/Windows.utm/Data/disk.qcow2", 2, 400);
    assert!(home.scan().files.is_empty());
}

#[test]
fn mail_messages_and_ios_backups_under_library_are_never_listed() {
    let home = Home::new();
    home.file("Library/Mail/V10/big.mbox", 2, 400);
    home.file("Library/Messages/Attachments/big.mov", 2, 400);
    home.file(
        "Library/Application Support/MobileSync/Backup/ab12/Manifest.db",
        2,
        400,
    );
    home.file("Library/Containers/com.app/Data/big.zip", 2, 400);
    assert!(home.scan().files.is_empty());
}

#[test]
fn a_file_inside_a_git_work_tree_is_never_listed() {
    let home = Home::new();
    home.file("Documents/repo/assets/big.mov", 2, 400);
    fs::create_dir_all(home.root.join("Documents/repo/.git")).unwrap();
    // A submodule or linked worktree has a `.git` file instead.
    home.file("Documents/linked/data/big.zip", 2, 400);
    fs::write(home.root.join("Documents/linked/.git"), "gitdir: elsewhere").unwrap();
    home.file("Documents/plain/big.zip", 2, 400);
    assert_eq!(home.names(&home.scan()), ["big"]);
    assert!(home.scan().files[0].path.ends_with("plain/big.zip"));
}

#[test]
fn a_home_that_holds_dotfiles_in_git_is_still_walked() {
    let home = Home::new();
    fs::create_dir_all(home.root.join(".git")).unwrap();
    home.file("Documents/big.zip", 2, 400);
    assert_eq!(home.scan().files.len(), 1);
}

#[test]
fn smart_cleans_categories_are_never_listed() {
    let home = Home::new();
    for path in [
        ".Trash/old.zip",
        ".cache/models/big.bin",
        ".cargo/registry/big.crate",
        "Documents/app/node_modules/big.zip",
        "Documents/app/target/debug/big.zip",
        "Documents/app/build/big.zip",
        "Library/Developer/Xcode/DerivedData/big.zip",
        "Documents/App/DerivedData/big.zip",
        "Documents/App/Pods/big.zip",
        "Documents/.hidden-big.zip",
    ] {
        home.file(path, 2, 400);
    }
    assert!(home.scan().files.is_empty());
}

#[test]
fn nothing_outside_the_home_is_reached_through_a_symlink() {
    let home = Home::new();
    let outside = home._dir.path().canonicalize().unwrap().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let mut out = fs::File::create(outside.join("big.zip")).unwrap();
    out.write_all(&vec![7u8; 2 * MB]).unwrap();
    std::os::unix::fs::symlink(&outside, home.root.join("Documents/link")).unwrap();
    std::os::unix::fs::symlink(outside.join("big.zip"), home.root.join("Documents/big.zip"))
        .unwrap();
    assert!(home.scan().files.is_empty());
}

#[test]
fn an_icloud_placeholder_is_never_listed() {
    // `SF_DATALESS` cannot be set without the kernel, so the flag test is
    // checked directly and the placeholder's shape (a big length with no
    // blocks on disk) through the walk.
    assert!(flags_are_dataless(0x4000_0000 | 0x20));
    assert!(!flags_are_dataless(0x20));
    let home = Home::new();
    let path = home.root.join("Documents/placeholder.mov");
    let file = fs::File::create(&path).unwrap();
    file.set_len(3_000 * MB as u64).unwrap();
    file.set_modified(home.now - DAY * 400).unwrap();
    assert!(home.scan().files.is_empty());
}

#[test]
fn a_hard_link_counts_once_and_a_file_that_frees_nothing_is_not_offered() {
    let home = Home::new();
    let first = home.file("Documents/a.zip", 2, 400);
    fs::hard_link(&first, home.root.join("Documents/b.zip")).unwrap();
    assert_eq!(home.scan().files.len(), 1);
    // A full APFS clone frees 0 bytes (the volume answers on macOS only).
    #[cfg(target_os = "macos")]
    {
        use std::os::unix::ffi::OsStrExt;
        extern "C" {
            fn clonefile(
                src: *const libc::c_char,
                dst: *const libc::c_char,
                flags: u32,
            ) -> libc::c_int;
        }
        let home = Home::new();
        let src = home.file("Documents/orig.zip", 2, 400);
        let dst = home.root.join("Documents/clone.zip");
        let c = |p: &Path| std::ffi::CString::new(p.as_os_str().as_bytes()).unwrap();
        let rc = unsafe { clonefile(c(&src).as_ptr(), c(&dst).as_ptr(), 0) };
        if rc == 0 {
            // The clone and the original share every block: neither frees it.
            assert!(home.scan().files.is_empty());
        }
    }
}

#[test]
fn a_folder_that_cannot_be_listed_is_reported_not_hidden() {
    use std::os::unix::fs::PermissionsExt;
    let home = Home::new();
    let locked = home.root.join("Documents/locked");
    fs::create_dir_all(&locked).unwrap();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o000)).unwrap();
    let scan = home.scan();
    fs::set_permissions(&locked, fs::Permissions::from_mode(0o755)).unwrap();
    // Root can list anything; only assert where the permission held.
    if fs::read_dir(&locked).is_ok() && scan.unreadable.is_empty() {
        return;
    }
    assert_eq!(scan.unreadable, [locked]);
}

#[test]
fn a_cancelled_walk_stops_and_a_finished_one_reports_done() {
    let home = Home::new();
    home.file("Documents/a.zip", 2, 400);
    let cancel = AtomicBool::new(true);
    let claimed = HashSet::new();
    let walk = HomeWalk {
        home: &home.root,
        limits: Limits {
            copy_min_bytes: MB as u64,
            only_copy_min_bytes: MB as u64,
            other_min_bytes: MB as u64,
            other_min_age_days: 90,
        },
        last_used: &NoLastUsed,
        now: home.now,
        claimed: &claimed,
    };
    assert!(scan_home(&walk, Some(&cancel), None).files.is_empty());
    let messages = Mutex::new(Vec::new());
    let on_progress = |p: crate::progress::Progress| messages.lock().unwrap().push(p.message);
    scan_home(&walk, None, Some(&on_progress));
    assert_eq!(
        messages.lock().unwrap().last().map(String::as_str),
        Some("Done")
    );
}

#[test]
fn the_default_limits_are_the_specs() {
    let limits = Limits::default();
    assert_eq!(limits.other_min_bytes, 500_000_000);
    assert_eq!(limits.only_copy_min_bytes, 500_000_000);
    assert_eq!(limits.other_min_age_days, 90);
}

#[test]
fn a_spotlight_date_parses_with_its_zone_and_null_is_none() {
    let at = |s| {
        parse_mdls_date(s)
            .unwrap()
            .duration_since(SystemTime::UNIX_EPOCH)
            .unwrap()
            .as_secs()
    };
    assert_eq!(at("1970-01-02 00:00:00 +0000"), 86_400);
    assert_eq!(at("2026-11-01 08:30:00 +0000"), 1_793_521_800);
    assert_eq!(at("2026-11-01 15:30:00 +0700"), 1_793_521_800);
    assert!(parse_mdls_date("(null)").is_none());
    assert!(parse_mdls_date("").is_none());
}
