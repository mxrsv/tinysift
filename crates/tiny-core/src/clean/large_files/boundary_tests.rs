//! The Large Files boundary of LV-L5 and LV-L6 on temp-dir fixtures with
//! 1 MB limits: who owns a path, the size filter, group 3's kinds, the
//! archive-unpacked and downloaded-from reasons, the lens, and group 5's
//! items an app owns. Nothing reads the real home.

use std::collections::HashSet;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use tempfile::TempDir;

use super::archive_beside::{extracted_beside, parse_directory, EntryKind};
use super::last_used::{LastUsed, NoLastUsed};
use super::owned::{owner_of_folder, scan_known_locations, OwnedItem};
use super::where_from::{binary_plist_strings, web_host};
use super::*;
use crate::clean::process::AppProbe;
use crate::clean::providers::downloads_old::list_old_files;
use crate::clean::scan_context::ScanContext;
use crate::runner::test_support::MockRunner;

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
        let root = dir.path().canonicalize().unwrap().join("home");
        fs::create_dir_all(root.join("Documents")).unwrap();
        Self {
            _dir: dir,
            root,
            now: SystemTime::now(),
        }
    }

    /// `megabytes` MB of varied bytes changed `days` ago, at `relative`.
    fn file(&self, relative: &str, megabytes: usize, days: u64) -> PathBuf {
        let block: Vec<u8> = (0..MB).map(|i| (i % 251) as u8).collect();
        self.write(relative, &block.repeat(megabytes), days)
    }

    fn write(&self, relative: &str, bytes: &[u8], days: u64) -> PathBuf {
        let path = self.root.join(relative);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let mut out = fs::File::create(&path).unwrap();
        out.write_all(bytes).unwrap();
        out.set_modified(self.now - DAY * days as u32).unwrap();
        path
    }

    fn limits(&self) -> Limits {
        Limits {
            copy_min_bytes: MB as u64,
            only_copy_min_bytes: MB as u64,
            other_min_bytes: MB as u64,
            other_min_age_days: 90,
            old_downloads_days: Some(30),
            owned_min_bytes: MB as u64,
            cloud_min_bytes: MB as u64,
        }
    }

    fn scan_with(&self, limits: Limits) -> HomeScan {
        self.scan_used(limits, &NoLastUsed)
    }

    fn scan_used(&self, limits: Limits, last_used: &dyn LastUsed) -> HomeScan {
        let claimed = HashSet::new();
        scan_home(
            &HomeWalk {
                home: &self.root,
                limits,
                last_used,
                now: self.now,
                claimed: &claimed,
            },
            None,
            None,
        )
    }

    fn scan(&self) -> HomeScan {
        self.scan_with(self.limits())
    }

    fn device(&self) -> u64 {
        use std::os::unix::fs::MetadataExt;
        fs::metadata(&self.root).unwrap().dev()
    }
}

fn row<'a>(scan: &'a HomeScan, path: &Path) -> Option<&'a LargeFile> {
    scan.files.iter().find(|f| f.path == path)
}

/// Spotlight says one file was opened at one time.
struct OpenedAt(PathBuf, SystemTime);

impl LastUsed for OpenedAt {
    fn last_used(&self, path: &Path) -> Option<SystemTime> {
        (path == self.0).then_some(self.1)
    }
}

// ---------- LV-L5: Smart Clean's category beats group 4 ----------

#[test]
fn an_old_download_old_downloads_lists_is_not_group_3_or_4() {
    let home = Home::new();
    let top = home.file("Downloads/export.bin", 2, 120);
    let nested = home.file("Downloads/kept/export.bin", 2, 120);
    let video = home.file("Downloads/talk.mp4", 2, 120);
    let recent_video = home.file("Downloads/new.mp4", 2, 5);
    let nested_video = home.file("Downloads/kept/talk.mp4", 2, 120);
    let archive = unpacked_archive(&home, "Downloads/Set.zip", 2);
    fs::File::options()
        .write(true)
        .open(&archive)
        .unwrap()
        .set_modified(home.now - DAY * 120)
        .unwrap();
    // The overlap was real: Smart Clean's own provider lists the file.
    let smart_clean: Vec<PathBuf> = list_old_files(
        &ScanContext::unchecked(),
        &MockRunner::new(),
        &home.root.join("Downloads"),
        30,
    )
    .into_iter()
    .map(|item| item.path)
    .collect();
    assert!(smart_clean.contains(&top) && smart_clean.contains(&video));
    let scan = home.scan();
    assert!(row(&scan, &top).is_none(), "Old Downloads owns it");
    // A kind is no more specific than age (LV-L5 (3)): Old Downloads owns it.
    assert!(row(&scan, &video).is_none(), "Old Downloads owns it too");
    // Old Downloads does not look into folders; groups 3 and 4 keep those.
    assert_eq!(row(&scan, &nested).unwrap().group, Group::OtherLarge);
    assert_eq!(row(&scan, &nested_video).unwrap().group, Group::OnlyCopy);
    assert_eq!(row(&scan, &recent_video).unwrap().group, Group::OnlyCopy);
    // An unpacked archive is the more specific reason: group 1 keeps it.
    assert_eq!(row(&scan, &archive).unwrap().group, Group::Copies);
    // Opened since, by Spotlight: Old Downloads does not list it, group 3 does.
    let opened = home.scan_used(home.limits(), &OpenedAt(video.clone(), home.now - DAY * 3));
    assert_eq!(row(&opened, &video).unwrap().group, Group::OnlyCopy);
    // Without the claim the same file is group 4's: the claim is the cause.
    let unclaimed = home.scan_with(Limits {
        old_downloads_days: None,
        ..home.limits()
    });
    assert_eq!(row(&unclaimed, &top).unwrap().group, Group::OtherLarge);
    assert_eq!(row(&unclaimed, &video).unwrap().group, Group::OnlyCopy);
}

#[test]
fn the_default_claim_uses_smart_cleans_own_days() {
    assert_eq!(
        Limits::default().old_downloads_days,
        Some(crate::options::CleanOptions::default().idle_days)
    );
}

// ---------- LV-L5: size floor and filter ----------

#[test]
fn the_size_filter_moves_groups_3_and_4_only_and_never_below_the_floor() {
    assert_eq!(SCAN_FLOOR_BYTES, 100_000_000);
    assert_eq!(SIZE_FILTER_BYTES, [100_000_000, 500_000_000, 1_000_000_000]);
    assert_eq!(DEFAULT_SIZE_FILTER_BYTES, Limits::default().other_min_bytes);
    assert!(Limits::with_size_filter(SCAN_FLOOR_BYTES - 1).is_none());
    for size in SIZE_FILTER_BYTES {
        let limits = Limits::with_size_filter(size).unwrap();
        assert_eq!(limits.only_copy_min_bytes, size);
        assert_eq!(limits.other_min_bytes, size);
        assert_eq!(limits.copy_min_bytes, COPY_MIN_BYTES);
        assert_eq!(limits.owned_min_bytes, SCAN_FLOOR_BYTES);
        assert_eq!(limits.cloud_min_bytes, DEFAULT_SIZE_FILTER_BYTES);
    }
    assert_eq!(INSTALLER_MIN_BYTES, SCAN_FLOOR_BYTES);
}

#[test]
fn a_raised_filter_hides_groups_3_and_4_but_keeps_copies() {
    let home = Home::new();
    let video = home.file("Documents/talk.mp4", 2, 5);
    let old = home.file("Documents/export.bin", 2, 200);
    let copy = unpacked_archive(&home, "Documents/Set.zip", 2);
    let scan = home.scan_with(Limits {
        only_copy_min_bytes: 3 * MB as u64,
        other_min_bytes: 3 * MB as u64,
        ..home.limits()
    });
    assert!(row(&scan, &video).is_none());
    assert!(row(&scan, &old).is_none());
    assert_eq!(row(&scan, &copy).unwrap().group, Group::Copies);
}

// ---------- LV-L6: group 3's kinds ----------

#[test]
fn group_3_holds_audio_disk_images_archives_and_dumps_whatever_their_age() {
    let home = Home::new();
    let cases = [
        ("Documents/mix.wav", "Audio · changed 1 day ago"),
        ("Documents/Ubuntu.iso", "Disk image · changed 1 day ago"),
        ("Documents/photos.zip", "Archive · changed 1 day ago"),
        ("Documents/prod.sql", "Database dump · changed 1 day ago"),
    ];
    for (path, _) in cases {
        home.file(path, 2, 1);
    }
    // A recent file of no such kind is in no group.
    let recent = home.file("Documents/export.bin", 2, 1);
    let scan = home.scan();
    for (path, reason) in cases {
        let found = row(&scan, &home.root.join(path)).unwrap();
        assert_eq!(found.group, Group::OnlyCopy, "{path}");
        assert_eq!(found.reason, reason);
    }
    assert!(row(&scan, &recent).is_none());
}

// ---------- LV-L5: the lens ----------

#[test]
fn every_row_carries_the_lens_its_kind_names() {
    for (name, lens) in [
        ("a.MOV", Lens::Video),
        ("a.dmg", Lens::Installer),
        ("a.pkg", Lens::Installer),
        ("a.tar.gz", Lens::Archive),
        ("a.qcow2", Lens::VirtualMachine),
        ("a.flac", Lens::Audio),
        ("a.psd", Lens::Other),
        ("noext", Lens::Other),
    ] {
        assert_eq!(Lens::of_path(Path::new(name)), lens, "{name}");
    }
    let ids: Vec<&str> = [
        Lens::Video,
        Lens::Installer,
        Lens::Archive,
        Lens::VirtualMachine,
        Lens::Audio,
        Lens::Other,
    ]
    .iter()
    .map(|l| l.id())
    .collect();
    assert_eq!(
        ids,
        [
            "video",
            "installer",
            "archive",
            "virtual_machine",
            "audio",
            "other"
        ]
    );
    let home = Home::new();
    let disk = home.file("Documents/old.vmdk", 2, 200);
    assert_eq!(row(&home.scan(), &disk).unwrap().lens, Lens::VirtualMachine);
}

// ---------- LV-L6: an archive unpacked beside it ----------

/// A stored (uncompressed) zip of `entries`; names ending in `/` are
/// folders. The CRC is left 0: the reader under test never checks it.
fn zip(entries: &[(&str, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for (name, data) in entries {
        let offset = out.len() as u32;
        let size = data.len() as u32;
        out.extend(0x0403_4b50u32.to_le_bytes());
        out.extend([20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        out.extend(0u32.to_le_bytes());
        out.extend(size.to_le_bytes());
        out.extend(size.to_le_bytes());
        out.extend((name.len() as u16).to_le_bytes());
        out.extend(0u16.to_le_bytes());
        out.extend(name.as_bytes());
        out.extend(data);
        let mode: u32 = if name.ends_with('/') {
            0o040_755
        } else {
            0o100_644
        };
        central.extend(0x0201_4b50u32.to_le_bytes());
        central.extend(((3u16 << 8) | 20).to_le_bytes());
        central.extend([20, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        central.extend(0u32.to_le_bytes());
        central.extend(size.to_le_bytes());
        central.extend(size.to_le_bytes());
        central.extend((name.len() as u16).to_le_bytes());
        central.extend([0, 0, 0, 0, 0, 0, 0, 0]);
        central.extend((mode << 16).to_le_bytes());
        central.extend(offset.to_le_bytes());
        central.extend(name.as_bytes());
    }
    let directory_at = out.len() as u32;
    out.extend(&central);
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend([0, 0, 0, 0]);
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend((entries.len() as u16).to_le_bytes());
    out.extend((central.len() as u32).to_le_bytes());
    out.extend(directory_at.to_le_bytes());
    out.extend(0u16.to_le_bytes());
    out
}

fn bytes(megabytes: usize, seed: u8) -> Vec<u8> {
    (0..megabytes * MB)
        .map(|i| (i % 251) as u8 ^ seed)
        .collect()
}

/// `relative` (a `.zip`) holding `Set/movie.bin` and `Set/notes.txt`, with
/// both unpacked beside it as Archive Utility leaves them.
fn unpacked_archive(home: &Home, relative: &str, megabytes: usize) -> PathBuf {
    let movie = bytes(megabytes, 1);
    let archive = home.write(
        relative,
        &zip(&[
            ("Set/", Vec::new()),
            ("Set/movie.bin", movie.clone()),
            ("Set/notes.txt", b"notes".to_vec()),
            ("__MACOSX/Set/._movie.bin", b"fork".to_vec()),
        ]),
        30,
    );
    let folder = archive.parent().unwrap().join("Set");
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("movie.bin"), movie).unwrap();
    fs::write(folder.join("notes.txt"), b"notes").unwrap();
    archive
}

#[test]
fn an_archive_whose_contents_sit_unpacked_beside_it_is_a_copy() {
    let home = Home::new();
    let archive = unpacked_archive(&home, "Documents/Set.zip", 2);
    let scan = home.scan();
    let found = row(&scan, &archive).unwrap();
    assert_eq!(found.group, Group::Copies);
    assert_eq!(found.reason, "Its contents sit unpacked beside it in Set");
    assert_eq!(found.lens, Lens::Archive);
    let extracted = found.extracted.as_ref().unwrap();
    assert_eq!(extracted.beside, home.root.join("Documents/Set"));
    // The resource fork is left out; the folder and both files count.
    assert_eq!(extracted.entries, 3);
    assert_eq!(extracted.anchor, home.root.join("Documents/Set/movie.bin"));
    // Listed once: the copy row left group 3.
    assert_eq!(scan.files.iter().filter(|f| f.path == archive).count(), 1);
}

#[test]
fn an_archives_unpacked_files_leave_groups_3_and_4_and_other_files_stay() {
    let home = Home::new();
    let talk = bytes(2, 7);
    let old = bytes(2, 8);
    let archive = home.write(
        "Documents/Set.zip",
        &zip(&[
            ("Set/", Vec::new()),
            ("Set/talk.mp4", talk.clone()),
            ("Set/old.bin", old.clone()),
        ]),
        5,
    );
    let unpacked_video = home.write("Documents/Set/talk.mp4", &talk, 5);
    let unpacked_old = home.write("Documents/Set/old.bin", &old, 200);
    // In the folder, but not one of the archive's files: still the only copy.
    let extra = home.file("Documents/Set/extra.mp4", 2, 5);
    let scan = home.scan();
    assert_eq!(row(&scan, &archive).unwrap().group, Group::Copies);
    assert!(
        row(&scan, &unpacked_video).is_none(),
        "the archive holds a copy"
    );
    assert!(
        row(&scan, &unpacked_old).is_none(),
        "the archive explains it"
    );
    assert_eq!(row(&scan, &extra).unwrap().group, Group::OnlyCopy);
    // Without the unpacked copy complete, nothing is a copy: both stay.
    fs::remove_file(&extra).unwrap();
    fs::write(&unpacked_old, b"edited").unwrap();
    let changed = home.scan();
    assert_eq!(row(&changed, &archive).unwrap().group, Group::OnlyCopy);
    assert_eq!(
        row(&changed, &unpacked_video).unwrap().group,
        Group::OnlyCopy
    );
}

#[test]
fn an_archive_of_several_items_unpacks_into_a_folder_named_after_it() {
    let home = Home::new();
    let a = bytes(2, 3);
    let archive = home.write(
        "Documents/Bundle.zip",
        &zip(&[("a.bin", a.clone()), ("b.txt", b"b".to_vec())]),
        5,
    );
    home.write("Documents/Bundle/a.bin", &a, 5);
    home.write("Documents/Bundle/b.txt", b"b", 5);
    let extracted = extracted_beside(&archive).unwrap();
    assert_eq!(extracted.beside, home.root.join("Documents/Bundle"));
}

#[test]
fn an_archive_is_no_copy_when_a_file_is_missing_or_differs() {
    let home = Home::new();
    let missing = unpacked_archive(&home, "Documents/one/Set.zip", 2);
    fs::remove_file(home.root.join("Documents/one/Set/notes.txt")).unwrap();
    let differs = unpacked_archive(&home, "Documents/two/Set.zip", 2);
    fs::write(home.root.join("Documents/two/Set/notes.txt"), b"edited!").unwrap();
    let alone = home.write(
        "Documents/three/Set.zip",
        &zip(&[("Set/x.bin", bytes(2, 5))]),
        5,
    );
    let scan = home.scan();
    for archive in [missing, differs, alone] {
        let found = row(&scan, &archive).unwrap();
        assert_eq!(found.group, Group::OnlyCopy, "{archive:?}");
        assert!(found.extracted.is_none());
    }
}

#[test]
fn a_name_that_leaves_its_folder_or_a_broken_directory_checks_nothing() {
    let home = Home::new();
    let escape = home.write("Documents/x.zip", &zip(&[("../evil.bin", vec![1])]), 5);
    home.write("evil.bin", &[1], 5);
    assert!(extracted_beside(&escape).is_none());
    let broken = home.write("Documents/broken.zip", b"PK not a zip", 5);
    assert!(extracted_beside(&broken).is_none());
}

#[test]
fn a_zip64_entry_reads_its_size_from_the_extra_field() {
    let name = b"big.bin";
    let mut entry = 0x0201_4b50u32.to_le_bytes().to_vec();
    entry.extend(((3u16 << 8) | 45).to_le_bytes());
    entry.extend([45, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
    entry.extend(0u32.to_le_bytes());
    entry.extend(0xFFFF_FFFFu32.to_le_bytes());
    entry.extend(0xFFFF_FFFFu32.to_le_bytes());
    entry.extend((name.len() as u16).to_le_bytes());
    entry.extend(20u16.to_le_bytes());
    // Comment length, start disk, internal attributes.
    entry.extend([0, 0, 0, 0, 0, 0]);
    entry.extend((0o100_644u32 << 16).to_le_bytes());
    entry.extend(0u32.to_le_bytes());
    entry.extend(name);
    entry.extend(0x0001u16.to_le_bytes());
    entry.extend(16u16.to_le_bytes());
    entry.extend(5_000_000_000u64.to_le_bytes());
    entry.extend(4_000_000_000u64.to_le_bytes());
    let entries = parse_directory(&entry, 1).unwrap();
    assert_eq!(entries[0].size, 5_000_000_000);
    assert_eq!(entries[0].kind, EntryKind::File);
}

// ---------- LV-L6: downloaded from ----------

/// A binary plist holding an array of ASCII strings.
fn where_froms(strings: &[&str]) -> Vec<u8> {
    let mut out = b"bplist00".to_vec();
    let mut offsets = vec![out.len()];
    out.push(0xA0 | strings.len() as u8);
    out.extend((1..=strings.len()).map(|i| i as u8));
    for text in strings {
        offsets.push(out.len());
        if text.len() < 15 {
            out.push(0x50 | text.len() as u8);
        } else {
            out.extend([0x5F, 0x10, text.len() as u8]);
        }
        out.extend(text.as_bytes());
    }
    let table = out.len();
    out.extend(offsets.iter().map(|o| *o as u8));
    out.extend([0, 0, 0, 0, 0, 0, 1, 1]);
    out.extend((offsets.len() as u64).to_be_bytes());
    out.extend(0u64.to_be_bytes());
    out.extend((table as u64).to_be_bytes());
    out
}

#[test]
fn a_where_from_list_parses_and_names_only_a_web_host() {
    let list = where_froms(&[
        "https://www.Example.com/files/a.dmg?token=x",
        "https://example.com/",
    ]);
    assert_eq!(
        binary_plist_strings(&list).unwrap(),
        [
            "https://www.Example.com/files/a.dmg?token=x",
            "https://example.com/"
        ]
    );
    assert_eq!(
        web_host("https://www.Example.com/a?x").as_deref(),
        Some("example.com")
    );
    assert_eq!(
        web_host("ftp://user@files.host.org:21/x").as_deref(),
        Some("files.host.org")
    );
    assert_eq!(web_host("message:%3Cid%3E"), None);
    assert_eq!(web_host("Sam's iPhone"), None);
    assert!(binary_plist_strings(b"bplist00").is_none());
    assert!(binary_plist_strings(&list[..list.len() - 1]).is_none());
}

#[cfg(target_os = "macos")]
fn set_where_from(path: &Path, value: &[u8]) {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let path = CString::new(path.as_os_str().as_bytes()).unwrap();
    let name = CString::new("com.apple.metadata:kMDItemWhereFroms").unwrap();
    // SAFETY: NUL-terminated strings and a buffer of the length passed.
    let done = unsafe {
        libc::setxattr(
            path.as_ptr(),
            name.as_ptr(),
            value.as_ptr().cast(),
            value.len(),
            0,
            0,
        )
    };
    assert_eq!(done, 0);
}

#[cfg(target_os = "macos")]
#[test]
fn a_row_says_where_it_was_downloaded_from() {
    let home = Home::new();
    let image = home.file("Documents/Tool.dmg", 2, 3);
    let old = home.file("Documents/deep/export.bin", 2, 200);
    for path in [&image, &old] {
        set_where_from(path, &where_froms(&["https://github.com/x/releases/a"]));
    }
    let scan = home.scan();
    let image = row(&scan, &image).unwrap();
    assert_eq!(
        image.reason,
        "Disk image from github.com · changed 3 days ago"
    );
    assert_eq!(image.downloaded_from.as_deref(), Some("github.com"));
    let old = row(&scan, &old).unwrap();
    assert_eq!(
        old.reason,
        "BIN file from github.com · not changed for 200 days"
    );
}

// ---------- LV-L5, LV-L6: group 5 items an app owns ----------

struct NoApps;

impl AppProbe for NoApps {
    fn probe(&self, _: &str) -> Result<bool, String> {
        Ok(false)
    }
}

fn owned_rows(home: &Home, owned: Vec<OwnedItem>, min_bytes: u64) -> Vec<LargeFile> {
    let applications = [home.root.join("Applications")];
    scan_elsewhere(
        &ElsewhereRoots {
            home: &home.root,
            applications: &applications,
            probe: &NoApps,
            owned_min_bytes: min_bytes,
        },
        Vec::new(),
        owned,
        None,
    )
}

#[test]
fn a_vm_or_disk_bundle_the_walk_meets_is_one_group_5_item_named_by_its_owner() {
    let home = Home::new();
    let pvm = home.root.join("Parallels/Windows 11.pvm");
    home.write("Parallels/Windows 11.pvm/config.pvs", b"<xml/>", 1);
    home.file("Parallels/Windows 11.pvm/harddisk.hdd/data.hds", 2, 200);
    let vmware = home.root.join("Virtual Machines.localized/Linux.vmwarevm");
    home.write(
        "Virtual Machines.localized/Linux.vmwarevm/Linux.vmx",
        b"x",
        1,
    );
    home.file(
        "Virtual Machines.localized/Linux.vmwarevm/disk.vmdk",
        2,
        200,
    );
    let vbox = home.root.join("VirtualBox VMs/Ubuntu");
    home.write("VirtualBox VMs/Ubuntu/Ubuntu.vbox", b"<xml/>", 1);
    home.file("VirtualBox VMs/Ubuntu/Ubuntu.vdi", 2, 200);
    let image = home.root.join("Documents/Vault.sparsebundle");
    home.write("Documents/Vault.sparsebundle/Info.plist", b"<plist/>", 1);
    home.file("Documents/Vault.sparsebundle/bands/0", 2, 200);
    // No marker: not this format's; still never entered or listed.
    home.file("Documents/Fake.pvm/disk.bin", 2, 200);
    let scan = home.scan();
    assert!(scan.files.is_empty(), "{:?}", scan.files);
    let mut owners: Vec<(PathBuf, String, &str)> = scan
        .owned
        .iter()
        .map(|i| (i.path.clone(), i.owner.app_name.clone(), i.kind))
        .collect();
    owners.sort();
    let mut expected = vec![
        (pvm, "Parallels Desktop".to_string(), "Virtual machine"),
        (vmware, "VMware Fusion".to_string(), "Virtual machine"),
        (vbox, "VirtualBox".to_string(), "Virtual machine"),
        (image, "Disk Utility".to_string(), "Disk image"),
    ];
    expected.sort();
    assert_eq!(owners, expected);
    let rows = owned_rows(&home, scan.owned, MB as u64);
    assert_eq!(rows.len(), 4);
    for row in &rows {
        assert_eq!(row.group, Group::Elsewhere);
        assert!(!row.group.movable());
        assert!(row.frees.bytes >= 2 * MB as u64);
    }
    let parallels = rows.iter().find(|r| r.name.contains("Parallels")).unwrap();
    assert_eq!(
        parallels.name,
        "Windows 11 · Parallels Desktop virtual machine"
    );
    assert_eq!(
        parallels.reason,
        "Remove it in Parallels Desktop, not here · a Parallels Desktop virtual machine"
    );
    assert_eq!(parallels.lens, Lens::VirtualMachine);
    let facts = parallels.elsewhere.as_ref().unwrap();
    assert_eq!(
        facts.app_bundle_id.as_deref(),
        Some("com.parallels.desktop.console")
    );
    assert!(facts.last_changed.is_some());
}

#[test]
fn an_item_under_the_floor_is_not_listed() {
    let home = Home::new();
    home.write("Parallels/Tiny.pvm/config.pvs", b"x", 1);
    let item = owner_of_folder(&home.root.join("Parallels/Tiny.pvm")).unwrap();
    assert!(owned_rows(&home, vec![item], MB as u64).is_empty());
}

#[test]
fn the_known_locations_list_dockers_disk_colima_backups_and_cloud_copies() {
    let home = Home::new();
    let docker = home.file(
        "Library/Containers/com.docker.docker/Data/vms/0/data/Docker.raw",
        2,
        1,
    );
    let colima = home.root.join(".colima/_lima/colima");
    home.file(".colima/_lima/colima/diffdisk", 2, 1);
    let data_disk = home.root.join(".colima/_lima/_disks/colima");
    home.file(".colima/_lima/_disks/colima/datadisk", 2, 1);
    home.write(".colima/_lima/_config/override.yaml", b"x", 1);
    let backup = home
        .root
        .join("Library/Application Support/MobileSync/Backup/00008110-AB");
    home.write(
        "Library/Application Support/MobileSync/Backup/00008110-AB/Info.plist",
        b"<plist><dict><key>Device Name</key>\n<string>Sam&apos;s iPhone</string></dict></plist>",
        1,
    );
    home.file(
        "Library/Application Support/MobileSync/Backup/00008110-AB/ab/cd",
        2,
        1,
    );
    let dropbox = home.file("Library/CloudStorage/Dropbox/Videos/trip.mov", 2, 1);
    let drive = home.file(
        "Library/CloudStorage/GoogleDrive-me@example.com/My Drive/db.bin",
        2,
        1,
    );
    home.file("Library/CloudStorage/Dropbox/small.bin", 0, 1);
    home.file("Library/CloudStorage/Dropbox/.hidden.bin", 2, 1);
    home.file("Library/CloudStorage/Dropbox/repo/assets/big.bin", 2, 1);
    fs::create_dir_all(home.root.join("Library/CloudStorage/Dropbox/repo/.git")).unwrap();
    let scan = scan_known_locations(&home.root, home.device(), MB as u64, None);
    assert!(scan.unreadable.is_empty());
    let by_path = |path: &Path| {
        scan.items
            .iter()
            .find(|i| i.path == path)
            .unwrap_or_else(|| panic!("{path:?} not listed: {:?}", scan.items))
    };
    assert_eq!(by_path(&docker).owner.app_name, "Docker Desktop");
    assert_eq!(by_path(&docker).lens, Lens::VirtualMachine);
    assert_eq!(by_path(&colima).name, "colima · Colima virtual machine");
    assert_eq!(by_path(&data_disk).name, "colima · Colima data disk");
    let phone = by_path(&backup);
    assert_eq!(phone.name, "Sam's iPhone · device backup");
    assert_eq!(phone.owner.app_name, "Finder");
    assert_eq!(by_path(&dropbox).owner.app_name, "Dropbox");
    assert_eq!(by_path(&dropbox).lens, Lens::Video);
    assert_eq!(
        by_path(&dropbox).reason,
        "Make it online-only in Dropbox, not here · it sits in Dropbox's synced folder"
    );
    assert_eq!(by_path(&drive).owner.app_name, "Google Drive");
    assert_eq!(scan.items.len(), 6, "{:?}", scan.items);
    // Group 5 rows, never movable.
    let rows = owned_rows(&home, scan.items, MB as u64);
    assert_eq!(rows.len(), 6);
    assert!(rows.iter().all(|r| !r.group.movable()));
}

#[cfg(unix)]
#[test]
fn a_known_location_that_cannot_be_listed_is_reported() {
    use std::os::unix::fs::PermissionsExt;
    let home = Home::new();
    let backups = home
        .root
        .join("Library/Application Support/MobileSync/Backup");
    fs::create_dir_all(&backups).unwrap();
    fs::set_permissions(&backups, fs::Permissions::from_mode(0o000)).unwrap();
    let scan = scan_known_locations(&home.root, home.device(), MB as u64, None);
    fs::set_permissions(&backups, fs::Permissions::from_mode(0o755)).unwrap();
    assert_eq!(scan.unreadable, [backups]);
    assert!(scan.items.is_empty());
}
