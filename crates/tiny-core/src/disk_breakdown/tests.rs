use super::places::{Class, Layout};
use super::volumes::{container_of, volume_name, Container, Volume};
use super::*;
use std::fs;
use std::io::Write;
use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::PathBuf;

const SIZE: usize = 64 * 1024;

fn layout() -> Layout {
    Layout {
        home: vec!["Users".into(), "me".into()],
    }
}

fn rel(path: &str) -> Vec<String> {
    path.split('/').map(String::from).collect()
}

fn place(path: &str, is_dir: bool) -> (DiskGroup, String, ItemKind) {
    match layout().classify(&rel(path), is_dir) {
        Class::Item(place) => (place.group, place.name, place.kind),
        Class::Descend => panic!("{path} descends"),
    }
}

fn descends(path: &str) -> bool {
    matches!(layout().classify(&rel(path), true), Class::Descend)
}

#[test]
fn paths_fall_into_the_system_settings_groups() {
    use DiskGroup::*;
    use ItemKind::*;
    let cases = [
        ("Applications/Cursor.app", Applications, "Cursor", App),
        (
            "Applications/Utilities",
            Applications,
            "/Applications/Utilities",
            Folder,
        ),
        (
            "Users/me/Applications/Chrome Apps.localized",
            Applications,
            "~/Applications/Chrome Apps.localized",
            Folder,
        ),
        ("Users/me/Downloads", Documents, "~/Downloads", Folder),
        ("Users/me/.Trash", Documents, "~/.Trash", Folder),
        ("Users/me/Library/Mail", Documents, "~/Library/Mail", Folder),
        (
            "Users/me/Library/Messages",
            Documents,
            "~/Library/Messages",
            Folder,
        ),
        (
            "Users/me/Library/Developer",
            Developer,
            "~/Library/Developer",
            Folder,
        ),
        ("Library/Developer", Developer, "/Library/Developer", Folder),
        ("Users/me/.cargo", Developer, "~/.cargo", Folder),
        (
            "Users/me/Library/Mobile Documents/com~apple~CloudDocs",
            ICloudDrive,
            "iCloud Drive",
            Folder,
        ),
        (
            "Users/me/Library/Mobile Documents/iCloud~md~obsidian",
            ICloudDrive,
            "obsidian",
            Folder,
        ),
        ("Users/Shared", OtherUsers, "/Users/Shared", Folder),
        ("Users/dev", OtherUsers, "/Users/dev", Folder),
        (
            "Users/me/Library/Containers/com.utmapp.UTM",
            SystemData,
            "com.utmapp.UTM",
            Container,
        ),
        (
            "Users/me/Library/Group Containers/6N38VWS5BX.ru.keepcoder.Telegram",
            SystemData,
            "6N38VWS5BX.ru.keepcoder.Telegram",
            Container,
        ),
        (
            "Users/me/Library/Caches",
            SystemData,
            "~/Library/Caches",
            Folder,
        ),
        ("Library/Caches", SystemData, "/Library", Folder),
        ("private/var", SystemData, "/private/var", Folder),
        ("opt", SystemData, "/opt", Folder),
    ];
    for (path, group, name, kind) in cases {
        assert_eq!(place(path, true), (group, name.to_string(), kind), "{path}");
    }
}

#[test]
fn a_path_below_an_item_belongs_to_that_item() {
    let (group, name, _) = place(
        "Users/me/Library/Containers/com.utmapp.UTM/Data/Documents/vm.utm",
        false,
    );
    assert_eq!(
        (group, name.as_str()),
        (DiskGroup::SystemData, "com.utmapp.UTM")
    );
    let (group, name, _) = place("Applications/Cursor.app/Contents/MacOS/Cursor", false);
    assert_eq!((group, name.as_str()), (DiskGroup::Applications, "Cursor"));
}

#[test]
fn shared_folders_descend_and_loose_files_name_themselves() {
    for path in [
        "Users",
        "Users/me",
        "Users/me/Library",
        "Users/me/Library/Containers",
        "Applications",
        "Library",
        "private",
    ] {
        assert!(descends(path), "{path}");
    }
    assert_eq!(
        place("Users/me/Library/.localized", false).1,
        "~/Library/.localized"
    );
    assert_eq!(place("Users/.localized", false).0, DiskGroup::OtherUsers);
}

#[test]
fn a_device_names_its_container() {
    assert_eq!(container_of("/dev/disk3s5"), Some("disk3"));
    assert_eq!(container_of("/dev/disk3s1s1"), Some("disk3"));
    assert_eq!(container_of("/dev/disk12s2"), Some("disk12"));
    assert_eq!(container_of("map auto_home"), None);
    assert_eq!(container_of("/dev/disk"), None);
    assert_eq!(volume_name("/"), "System volume");
    assert_eq!(volume_name("/System/Volumes/Preboot"), "Preboot");
}

/// A Data volume in a temp folder, with the home at `Users/me`.
struct Fixture {
    dir: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().unwrap(),
        }
    }

    fn root(&self) -> PathBuf {
        self.dir.path().to_path_buf()
    }

    fn file(&self, path: &str, bytes: usize) -> PathBuf {
        let path = self.root().join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut file = fs::File::create(&path).unwrap();
        file.write_all(&vec![0xA5; bytes]).unwrap();
        file.sync_all().unwrap();
        path
    }

    fn container(&self, data_used: u64) -> Container {
        Container {
            total: 100 << 30,
            available: (100 << 30) - data_used - (300 << 20),
            data: Volume {
                mount: "/System/Volumes/Data".into(),
                used: data_used,
            },
            data_files: 0,
            others: vec![
                Volume {
                    mount: "/".into(),
                    used: 200 << 20,
                },
                Volume {
                    mount: "/System/Volumes/VM".into(),
                    used: 60 << 20,
                },
                Volume {
                    mount: "/System/Volumes/Preboot".into(),
                    used: 30 << 20,
                },
            ],
        }
    }

    fn measure(&self, container: &Container) -> DiskBreakdown {
        measure_with(&self.root(), Path::new("/Users/me"), container, None, None).unwrap()
    }
}

fn allocated(path: &Path) -> u64 {
    fs::symlink_metadata(path).unwrap().blocks() * 512
}

fn group(report: &DiskBreakdown, group: DiskGroup) -> &GroupReport {
    report.groups.iter().find(|g| g.group == group).unwrap()
}

#[test]
fn groups_and_the_gap_add_up_to_the_used_space() {
    let fx = Fixture::new();
    let files = [
        fx.file("Applications/Foo.app/Contents/MacOS/Foo", SIZE),
        fx.file("Users/me/Downloads/a.zip", 2 * SIZE),
        fx.file("Users/me/.cargo/registry/x", SIZE),
        fx.file(
            "Users/me/Library/Containers/com.example.app/Data/f",
            3 * SIZE,
        ),
        fx.file("Users/me/Library/Caches/c", SIZE),
        fx.file(
            "Users/me/Library/Mobile Documents/com~apple~CloudDocs/d",
            SIZE,
        ),
        fx.file("Users/Shared/s", SIZE),
        fx.file("opt/o", SIZE),
    ];
    let walked: u64 = files.iter().map(|path| allocated(path)).sum();
    let gap = 5 << 20;
    let report = fx.measure(&fx.container(walked + gap));
    assert_eq!(
        report.used_bytes,
        report.measured_bytes + report.not_measured_bytes
    );
    assert_eq!(
        report.groups.iter().map(|g| g.bytes).sum::<u64>(),
        report.measured_bytes
    );
    // The volumes add 290 MB and the container 10 MB more (`Recovery and other volumes`), so only the Data volume's
    // unwalked bytes are left.
    assert_eq!(report.not_measured_bytes, gap);
    assert!(!report.inexact);
    assert_eq!(group(&report, DiskGroup::Applications).items[0].name, "Foo");
    assert_eq!(
        group(&report, DiskGroup::Documents).bytes,
        allocated(&files[1])
    );
    assert_eq!(
        group(&report, DiskGroup::ICloudDrive).items[0].name,
        "iCloud Drive"
    );
    let system = group(&report, DiskGroup::SystemData);
    assert!(system
        .items
        .iter()
        .any(|item| item.name == "com.example.app" && item.kind == ItemKind::Container));
    assert!(system
        .items
        .iter()
        .any(|item| item.name == "Swap (VM volume)" && item.bytes == 60 << 20));
    let macos = group(&report, DiskGroup::MacOs);
    assert_eq!(macos.items[0].name, "System volume");
}

#[test]
fn a_hard_link_counts_once_in_the_first_group() {
    let fx = Fixture::new();
    let original = fx.file("opt/data/big", 4 * SIZE);
    fs::create_dir_all(fx.root().join("Users/me/Downloads")).unwrap();
    fs::hard_link(&original, fx.root().join("Users/me/Downloads/big")).unwrap();
    let report = fx.measure(&fx.container(allocated(&original)));
    assert_eq!(
        group(&report, DiskGroup::Documents).bytes,
        allocated(&original)
    );
    let opt = group(&report, DiskGroup::SystemData)
        .items
        .iter()
        .find(|item| item.name == "/opt")
        .map_or(0, |item| item.bytes);
    assert_eq!(opt, 0);
}

#[cfg(target_os = "macos")]
#[test]
fn a_clone_family_counts_once_in_the_first_group() {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let fx = Fixture::new();
    let original = fx.file("Applications/Big.app/Contents/blob", 8 * SIZE);
    let clone = fx.root().join("private/var/folders/x/blob");
    fs::create_dir_all(clone.parent().unwrap()).unwrap();
    let (from, to) = (
        CString::new(original.as_os_str().as_bytes()).unwrap(),
        CString::new(clone.as_os_str().as_bytes()).unwrap(),
    );
    // SAFETY: both are NUL-terminated paths that outlive the call.
    if unsafe { libc::clonefile(from.as_ptr(), to.as_ptr(), 0) } != 0 {
        eprintln!("skipped: the temp dir is not on APFS");
        return;
    }
    let report = fx.measure(&fx.container(allocated(&original)));
    assert_eq!(
        group(&report, DiskGroup::Applications).bytes,
        allocated(&original)
    );
    let var = group(&report, DiskGroup::SystemData)
        .items
        .iter()
        .find(|item| item.name == "/private/var")
        .map_or(0, |item| item.bytes);
    assert_eq!(var, 0, "the clone in /private/var frees nothing on its own");
}

#[test]
fn the_gap_is_never_negative() {
    let fx = Fixture::new();
    let file = fx.file("Users/me/Downloads/a", 4 * SIZE);
    let mut container = fx.container(0);
    container.available = container.total - (200 << 20) - (60 << 20) - (30 << 20);
    let report = fx.measure(&container);
    assert!(allocated(&file) > 0);
    assert_eq!(report.not_measured_bytes, 0);
    assert!(report.inexact);
    assert_eq!(report.measured_bytes, report.used_bytes);
}

#[test]
fn unreadable_folders_are_sorted_by_why() {
    if unsafe { libc::geteuid() } == 0 {
        eprintln!("skipped: root reads every folder");
        return;
    }
    let fx = Fixture::new();
    fx.file("Users/me/Library/Caches/locked/x", SIZE);
    fx.file("Users/dev/Documents/y", SIZE);
    let locked = [
        fx.root().join("Users/me/Library/Caches/locked"),
        fx.root().join("Users/dev"),
    ];
    for dir in &locked {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o000)).unwrap();
    }
    let report = fx.measure(&fx.container(1 << 20));
    for dir in &locked {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let reason = |refusal| {
        report
            .refused
            .iter()
            .find(|r| r.refusal == refusal)
            .cloned()
    };
    let system = reason(Refusal::System).expect("system refusal");
    assert_eq!(system.paths, vec!["~/Library/Caches/locked".to_string()]);
    let other = reason(Refusal::OtherAccount).expect("other account refusal");
    assert_eq!(other.paths, vec!["/Users/dev".to_string()]);
    assert!(reason(Refusal::Privacy).is_none());
}

#[test]
fn a_cancelled_walk_says_so() {
    let fx = Fixture::new();
    fx.file("Users/me/Downloads/a", SIZE);
    let cancel = AtomicBool::new(true);
    let report = measure_with(
        &fx.root(),
        Path::new("/Users/me"),
        &fx.container(1 << 20),
        Some(&cancel),
        None,
    )
    .unwrap();
    assert!(report.cancelled);
}

/// The owner's Mac, read-only: `cargo test -p tiny-core disk_breakdown::tests::this_mac -- --ignored --nocapture`.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "walks the whole startup disk"]
fn this_mac() {
    let started = std::time::Instant::now();
    let report = measure(None, None).unwrap();
    let gb = |bytes: u64| bytes as f64 / 1e9;
    println!(
        "used {:.1} GB = measured {:.1} + not measured {:.1} ({} entries, {:.0?})",
        gb(report.used_bytes),
        gb(report.measured_bytes),
        gb(report.not_measured_bytes),
        report.entries_walked,
        started.elapsed()
    );
    for group in &report.groups {
        println!("{:?} {:.1} GB", group.group, gb(group.bytes));
        for item in group.items.iter().take(6) {
            println!(
                "    {:<50} {:>8.2} GB {:?}",
                item.name,
                gb(item.bytes),
                item.kind
            );
        }
    }
    for refused in &report.refused {
        println!(
            "{:?} x{}: {:?}",
            refused.refusal, refused.count, refused.paths
        );
    }
    assert_eq!(
        report.measured_bytes + report.not_measured_bytes,
        report.used_bytes
    );
}
