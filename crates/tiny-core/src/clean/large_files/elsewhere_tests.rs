//! Group 5 on temp-dir fixtures: a fake UTM library with one VM, a fake Cap
//! library, `clonefile` pairs and a fake running-app probe. Nothing reads the
//! real home or Applications.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::SystemTime;

use tempfile::TempDir;

use super::discovery::{scan_home, HomeWalk, Limits};
use super::elsewhere::{scan_elsewhere, ElsewhereRoots, LIBRARIES};
use super::installers::{scan_installers_above, InstallerRoots};
use super::last_used::NoLastUsed;
use super::*;
use crate::clean::process::AppProbe;

const MB: usize = 1 << 20;
const UTM_FOLDER: &str = "Library/Containers/com.utmapp.UTM/Data/Documents";
const CAP_FOLDER: &str = "Library/Application Support/so.cap.desktop/recordings";

struct Probe(HashMap<&'static str, Result<bool, String>>);

impl AppProbe for Probe {
    fn probe(&self, name: &str) -> Result<bool, String> {
        self.0.get(name).cloned().unwrap_or(Ok(false))
    }
}

struct World {
    _dir: TempDir,
    home: PathBuf,
    apps: PathBuf,
}

impl World {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().canonicalize().unwrap();
        let (home, apps) = (base.join("home"), base.join("apps"));
        for folder in ["Downloads", "Documents"] {
            fs::create_dir_all(home.join(folder)).unwrap();
        }
        fs::create_dir_all(&apps).unwrap();
        Self {
            _dir: dir,
            home,
            apps,
        }
    }

    fn write(&self, relative: &str, megabytes: usize) -> PathBuf {
        let path = self.home.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let block: Vec<u8> = (0..MB).map(|i| (i % 251) as u8).collect();
        let mut out = fs::File::create(&path).unwrap();
        for _ in 0..megabytes {
            out.write_all(&block).unwrap();
        }
        path
    }

    fn utm_vm(&self, folder: &str, name_in_plist: &str, disk_mb: usize) -> PathBuf {
        let bundle = self.home.join(UTM_FOLDER).join(format!("{folder}.utm"));
        fs::create_dir_all(&bundle).unwrap();
        fs::write(
            bundle.join("config.plist"),
            format!(
                "<?xml version=\"1.0\"?><plist><dict><key>Backend</key><string>QEMU</string>\
                 <key>Information</key><dict><key>Icon</key><string>windows</string>\
                 <key>Name</key><string>{name_in_plist}</string></dict>\
                 <key>System</key><dict><key>Name</key><string>wrong</string></dict></dict></plist>"
            ),
        )
        .unwrap();
        fs::write(bundle.join("screenshot.png"), b"png").unwrap();
        self.write(
            &format!("{UTM_FOLDER}/{folder}.utm/Data/disk.qcow2"),
            disk_mb,
        );
        bundle
    }

    fn cap_recording(&self, stem: &str, with_meta: Option<&str>, mb: usize) -> PathBuf {
        let bundle = self.home.join(CAP_FOLDER).join(format!("{stem}.cap"));
        self.write(
            &format!("{CAP_FOLDER}/{stem}.cap/content/segments/segment-0/display.mp4"),
            mb,
        );
        if let Some(pretty) = with_meta {
            fs::write(
                bundle.join("recording-meta.json"),
                format!("{{\"pretty_name\": \"{pretty}\"}}"),
            )
            .unwrap();
        }
        bundle
    }

    fn scan(&self, probe: &Probe, frees_nothing: Vec<FreesNothing>) -> Vec<LargeFile> {
        let applications = [self.apps.clone()];
        scan_elsewhere(
            &ElsewhereRoots {
                home: &self.home,
                applications: &applications,
                probe,
            },
            frees_nothing,
            None,
        )
    }

    /// What the real scans hand over for FN1, with 1 MB thresholds.
    fn frees_nothing(&self) -> Vec<FreesNothing> {
        let roots = InstallerRoots {
            folders: vec![self.home.join("Downloads")],
            applications: vec![self.apps.clone()],
        };
        struct NoVersions;
        impl super::app_version::AppVersions for NoVersions {
            fn version(&self, _: &Path) -> Option<String> {
                None
            }
        }
        let mut found = scan_installers_above(MB as u64, &roots, &NoVersions, None).frees_nothing;
        let claimed = HashSet::new();
        found.extend(
            scan_home(
                &HomeWalk {
                    home: &self.home,
                    limits: Limits {
                        only_copy_min_bytes: MB as u64,
                        other_min_bytes: MB as u64,
                        other_min_age_days: 90,
                    },
                    last_used: &NoLastUsed,
                    now: SystemTime::now(),
                    claimed: &claimed,
                },
                None,
                None,
            )
            .frees_nothing,
        );
        found
    }
}

fn quiet() -> Probe {
    Probe(HashMap::new())
}

/// `false` when the volume cannot clone (not APFS): the test then has nothing to show.
#[cfg(target_os = "macos")]
fn clone_file(from: &Path, to: &Path) -> bool {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let from = CString::new(from.as_os_str().as_bytes()).unwrap();
    let to = CString::new(to.as_os_str().as_bytes()).unwrap();
    // SAFETY: valid NUL-terminated paths for the duration of the call.
    unsafe { libc::clonefile(from.as_ptr(), to.as_ptr(), 0) == 0 }
}

#[test]
fn the_table_holds_utm_and_cap_with_their_bundle_ids() {
    let ids: Vec<_> = LIBRARIES
        .iter()
        .map(|l| (l.app_name, l.bundle_id))
        .collect();
    assert_eq!(
        ids,
        vec![("UTM", "com.utmapp.UTM"), ("Cap", "so.cap.desktop")]
    );
}

#[test]
fn a_utm_vm_is_named_from_its_plist_and_frees_what_it_holds() {
    let world = World::new();
    let bundle = world.utm_vm("Win-folder", "Win &amp; Co", 3);
    let rows = world.scan(&quiet(), vec![]);
    assert_eq!(rows.len(), 1);
    let vm = &rows[0];
    assert_eq!(vm.group, Group::Elsewhere);
    assert_eq!(vm.path, bundle);
    assert_eq!(vm.name, "Win & Co · UTM virtual machine");
    assert!(vm.frees.bytes >= 3 * MB as u64);
    assert_eq!(
        vm.reason,
        "Remove it in UTM, not here · it sits in UTM's library"
    );
    let facts = vm.elsewhere.as_ref().unwrap();
    assert_eq!(facts.kind, "Virtual machine");
    assert_eq!(facts.app_name.as_deref(), Some("UTM"));
    assert_eq!(facts.app_bundle_id.as_deref(), Some("com.utmapp.UTM"));
    assert_eq!(facts.preview, Some(bundle.join("screenshot.png")));
    assert!(facts.shared_bytes.is_none());
    assert!(facts.last_changed.is_some());
}

#[test]
fn a_vm_without_a_readable_name_falls_back_to_its_bundle_name() {
    let world = World::new();
    let bundle = world.home.join(UTM_FOLDER).join("Plain.utm");
    fs::create_dir_all(&bundle).unwrap();
    fs::write(bundle.join("config.plist"), b"bplist00").unwrap();
    let rows = world.scan(&quiet(), vec![]);
    assert_eq!(rows[0].name, "Plain · UTM virtual machine");
    assert!(rows[0].elsewhere.as_ref().unwrap().preview.is_none());
}

#[test]
fn a_cap_recording_is_named_by_its_date_from_the_bundle_or_the_meta_file() {
    let world = World::new();
    let by_name = world.cap_recording("DELL U2723QE (1) (Display) 2026-10-30 05.10 PM", None, 2);
    world.cap_recording("c", Some("Cap 2026-10-30 at 17.40.53"), 1);
    world.cap_recording("other", None, 1);
    fs::create_dir_all(by_name.join("screenshots")).unwrap();
    fs::write(by_name.join("screenshots/thumbnail-1.jpg"), b"j").unwrap();
    fs::write(by_name.join("screenshots/display.jpg"), b"j").unwrap();
    let rows = world.scan(&quiet(), vec![]);
    let names: Vec<_> = rows.iter().map(|r| r.name.as_str()).collect();
    assert_eq!(
        names,
        vec![
            "Cap recording · 30 Oct, 5:10 PM",
            "Cap recording · 30 Oct, 5:40 PM",
            "Cap recording · other",
        ]
    );
    let first = rows[0].elsewhere.as_ref().unwrap();
    assert_eq!(first.kind, "Screen recording");
    assert_eq!(first.preview, Some(by_name.join("screenshots/display.jpg")));
    assert!(rows[1].elsewhere.as_ref().unwrap().preview.is_none());
}

#[test]
fn the_owning_app_is_found_and_its_running_state_comes_from_the_probe() {
    let world = World::new();
    world.utm_vm("Win", "Windows", 1);
    world.cap_recording("x 2026-10-30 03.50 PM", None, 1);
    fs::create_dir_all(world.apps.join("UTM.app")).unwrap();
    let probe = Probe(HashMap::from([
        ("UTM", Ok(true)),
        ("Cap", Err("pgrep failed".to_string())),
    ]));
    let rows = world.scan(&probe, vec![]);
    let by_app: HashMap<_, _> = rows
        .iter()
        .map(|r| (r.elsewhere.as_ref().unwrap().app_name.clone().unwrap(), r))
        .collect();
    let utm = by_app["UTM"];
    assert_eq!(utm.matched_app, Some(world.apps.join("UTM.app")));
    assert!(utm.elsewhere.as_ref().unwrap().app_running);
    let cap = by_app["Cap"];
    assert!(
        cap.matched_app.is_none(),
        "Cap is not installed in the fixture"
    );
    assert!(
        !cap.elsewhere.as_ref().unwrap().app_running,
        "unknown is not running"
    );
}

#[test]
fn a_cancelled_scan_lists_nothing_it_did_not_reach() {
    let world = World::new();
    world.utm_vm("Win", "Windows", 1);
    let flag = AtomicBool::new(true);
    let probe = quiet();
    let applications = [world.apps.clone()];
    let rows = scan_elsewhere(
        &ElsewhereRoots {
            home: &world.home,
            applications: &applications,
            probe: &probe,
        },
        vec![],
        Some(&flag),
    );
    assert!(rows.is_empty());
}

#[cfg(target_os = "macos")]
#[test]
fn a_clone_of_a_vms_file_frees_nothing_and_the_reason_names_the_vm() {
    let world = World::new();
    let bundle = world.utm_vm("Win", "Windows", 1);
    let installer = world.write("Downloads/Windows11.iso", 3);
    let inside = bundle.join("Data/Windows11.iso");
    if !clone_file(&installer, &inside) {
        return; // not APFS
    }
    let rows = world.scan(&quiet(), world.frees_nothing());
    let iso = rows.iter().find(|r| r.path == installer).expect("listed");
    assert_eq!(iso.group, Group::Elsewhere);
    assert_eq!(iso.frees.bytes, 0);
    // No number in a reason: the app writes sizes in the user's locale.
    assert_eq!(
        iso.reason,
        "Frees nothing: UTM's Windows VM holds the same data"
    );
    let facts = iso.elsewhere.as_ref().unwrap();
    assert_eq!(facts.kind, "Installer");
    assert_eq!(facts.app_name.as_deref(), Some("UTM"));
    assert_eq!(facts.shared_with.as_deref(), Some("UTM's Windows VM"));
    assert!(facts.shared_bytes.unwrap() >= 3 * MB as u64);
    // The VM itself is still one row, and the ISO is not listed twice.
    assert_eq!(rows.iter().filter(|r| r.path == installer).count(), 1);
    assert!(rows.iter().any(|r| r.path == bundle));
}

#[cfg(target_os = "macos")]
#[test]
fn a_clone_with_no_partner_in_a_library_says_only_that_another_copy_uses_the_space() {
    let world = World::new();
    let original = world.write("Documents/orig.bin", 2);
    if !clone_file(&original, &world.home.join("Documents/copy.bin")) {
        return; // not APFS
    }
    let rows = world.scan(&quiet(), world.frees_nothing());
    assert_eq!(rows.len(), 2, "the original frees nothing as well");
    for row in &rows {
        assert_eq!(
            row.reason,
            "Frees nothing: another copy on this Mac uses the same space"
        );
        let facts = row.elsewhere.as_ref().unwrap();
        assert_eq!(facts.kind, "File");
        assert!(facts.app_name.is_none() && facts.shared_with.is_none());
    }
}

#[test]
fn a_plain_large_file_is_not_a_file_that_frees_nothing() {
    let world = World::new();
    world.write("Documents/plain.bin", 2);
    assert!(world.frees_nothing().is_empty());
}

#[test]
fn sizes_read_as_the_app_writes_them() {
    let world = World::new();
    let file = FreesNothing {
        path: world.write("Documents/x.bin", 1),
        allocated: 8_530_000_000,
        modified: None,
        fingerprint: PathFingerprint::of(
            &fs::symlink_metadata(world.home.join("Documents/x.bin")).unwrap(),
        ),
    };
    let rows = world.scan(&quiet(), vec![file]);
    assert_eq!(
        rows[0].elsewhere.as_ref().unwrap().shared_bytes,
        Some(8_530_000_000)
    );
    assert!(rows[0].elsewhere.as_ref().unwrap().kind == "File");
}
