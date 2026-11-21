//! The LV-L5 / LV-L6 boundary through the adapter: the size filter call, the
//! records' new fields, group 5 items an app owns, Old Downloads' claim and
//! the consent rules of an archive's Copies row.

use super::*;
use tiny_core::clean::large_files::last_used::NoLastUsed;

fn file(world: &World, relative: &str, bytes: &[u8], days: u64) -> PathBuf {
    let path = world.home.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(&path, bytes).unwrap();
    let handle = fs::File::options().write(true).open(&path).unwrap();
    handle
        .set_modified(std::time::SystemTime::now() - Duration::from_secs(86_400 * days))
        .unwrap();
    path
}

fn big(seed: u8) -> Vec<u8> {
    (0..2 * MB).map(|i| (i % 251) as u8 ^ seed).collect()
}

fn scan(world: &World) -> FfiLargeFilesScan {
    let limits = Limits {
        copy_min_bytes: MB as u64,
        only_copy_min_bytes: MB as u64,
        other_min_bytes: MB as u64,
        other_min_age_days: 90,
        old_downloads_days: Some(30),
        owned_min_bytes: MB as u64,
        cloud_min_bytes: MB as u64,
    };
    world
        .session
        .scan_large_files_within(
            &world.roots(),
            MB as u64,
            &NoVersions,
            Some(HomeSettings {
                home: &world.home,
                limits,
                last_used: &NoLastUsed,
                probe: &NoApps,
            }),
            None,
            &CancellationToken::default(),
            &Listener::default(),
        )
        .unwrap()
}

fn by_path<'a>(scan: &'a FfiLargeFilesScan, path: &Path) -> Option<&'a FfiLargeFile> {
    scan.files.iter().find(|f| f.path == path.to_string_lossy())
}

/// A stored zip holding `Set/movie.bin` (2 MB) and `Set/notes.txt`, both
/// unpacked beside it. CRCs are 0: only the central directory is read.
fn unpacked_archive(world: &World, relative: &str) -> PathBuf {
    let movie = big(1);
    let entries: [(&str, &[u8]); 3] = [
        ("Set/", b""),
        ("Set/movie.bin", &movie),
        ("Set/notes.txt", b"notes"),
    ];
    let (mut out, mut central) = (Vec::new(), Vec::new());
    for (name, data) in entries {
        let offset = out.len() as u32;
        let size = (data.len() as u32).to_le_bytes();
        out.extend(0x0403_4b50u32.to_le_bytes());
        out.extend([20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        out.extend(size);
        out.extend(size);
        out.extend((name.len() as u16).to_le_bytes());
        out.extend([0, 0]);
        out.extend(name.as_bytes());
        out.extend(data);
        central.extend(0x0201_4b50u32.to_le_bytes());
        central.extend([20, 3, 20, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
        central.extend(size);
        central.extend(size);
        central.extend((name.len() as u16).to_le_bytes());
        central.extend([0; 12]);
        central.extend(offset.to_le_bytes());
        central.extend(name.as_bytes());
    }
    let at = out.len() as u32;
    out.extend(&central);
    out.extend(0x0605_4b50u32.to_le_bytes());
    out.extend([0, 0, 0, 0, 3, 0, 3, 0]);
    out.extend((central.len() as u32).to_le_bytes());
    out.extend(at.to_le_bytes());
    out.extend([0, 0]);
    let archive = file(world, relative, &out, 30);
    let folder = archive.parent().unwrap().join("Set");
    fs::create_dir_all(&folder).unwrap();
    fs::write(folder.join("movie.bin"), movie).unwrap();
    fs::write(folder.join("notes.txt"), b"notes").unwrap();
    archive
}

#[test]
fn the_size_filters_come_from_core() {
    let filters = large_files_size_filters();
    assert_eq!(filters.floor_bytes, 100_000_000);
    assert_eq!(
        filters.choices_bytes,
        [100_000_000, 500_000_000, 1_000_000_000]
    );
    assert_eq!(filters.default_bytes, 500_000_000);
}

#[test]
fn a_filter_under_the_floor_or_a_root_outside_the_home_scans_nothing() {
    let world = World::new();
    let call = |options| {
        world.session.large_files_scan_with(
            options,
            Arc::new(CancellationToken::default()),
            Arc::new(Listener::default()),
        )
    };
    let under = call(FfiLargeFilesScanOptions {
        root: None,
        groups_3_4_min_bytes: 99_999_999,
    });
    assert!(
        matches!(under, Err(FfiError::InvalidInput { .. })),
        "{under:?}"
    );
    let outside = call(FfiLargeFilesScanOptions {
        root: Some(std::env::temp_dir().to_string_lossy().into_owned()),
        groups_3_4_min_bytes: 500_000_000,
    });
    assert!(
        matches!(outside, Err(FfiError::InvalidInput { .. })),
        "{outside:?}"
    );
    assert!(world.session.large_files_state().scan.is_none());
}

#[test]
fn records_carry_the_lens_and_old_downloads_keeps_its_own_files() {
    let world = World::new();
    let claimed = file(&world, "Downloads/export.bin", &big(2), 120);
    let audio = file(&world, "Documents/mix.wav", &big(3), 2);
    let other = file(&world, "Documents/deep/export.bin", &big(4), 200);
    let scan = scan(&world);
    assert!(by_path(&scan, &claimed).is_none());
    let audio = by_path(&scan, &audio).unwrap();
    assert_eq!(
        (audio.group.as_str(), audio.lens.as_str()),
        ("only_copy", "audio")
    );
    let other = by_path(&scan, &other).unwrap();
    assert_eq!(
        (other.group.as_str(), other.lens.as_str()),
        ("other_large", "other")
    );
    assert!(other.downloaded_from.is_none() && other.extracted_path.is_none());
}

#[test]
fn a_vm_the_walk_meets_is_a_group_5_row_the_preview_refuses() {
    let world = World::new();
    let vm = world.home.join("Parallels/Win.pvm");
    file(&world, "Parallels/Win.pvm/config.pvs", b"x", 1);
    file(&world, "Parallels/Win.pvm/disk.hds", &big(5), 1);
    let scan = scan(&world);
    let row = by_path(&scan, &vm).unwrap();
    assert_eq!(row.group, "elsewhere");
    assert_eq!(row.lens, "virtual_machine");
    assert_eq!(row.kind.as_deref(), Some("Virtual machine"));
    assert_eq!(row.app_name.as_deref(), Some("Parallels Desktop"));
    assert_eq!(scan.files.len(), 1, "the disk inside is not a row");
    assert!(matches!(
        world
            .session
            .preview_large_files(std::slice::from_ref(&row.id)),
        Err(FfiError::InvalidInput { .. })
    ));
}

#[test]
fn an_unpacked_archive_moves_alone_and_only_while_its_copy_stays() {
    let world = World::new();
    let archive = unpacked_archive(&world, "Documents/Set.zip");
    // Old and large enough for group 4 itself, inside the unpacked copy.
    let inside = file(&world, "Documents/Set/old.bin", &big(6), 200);
    let scan = scan(&world);
    let row = by_path(&scan, &archive).unwrap();
    assert_eq!(row.group, "copies");
    assert_eq!(row.lens, "archive");
    assert_eq!(
        row.extracted_path.as_deref(),
        Some(world.home.join("Documents/Set").to_str().unwrap())
    );
    assert_eq!(row.extracted_entries, Some(3));
    assert!(
        row.keeper_path.is_none(),
        "a video copy's fields stay empty"
    );
    // Never together with a file of the copy that stays.
    let inside = by_path(&scan, &inside).unwrap();
    assert_eq!(inside.group, "other_large");
    assert!(matches!(
        world
            .session
            .preview_large_files(&[row.id.clone(), inside.id.clone()]),
        Err(FfiError::InvalidInput { .. })
    ));
    // Alone it previews; once the unpacked copy changed it stays put.
    let preview = world
        .session
        .preview_large_files(std::slice::from_ref(&row.id))
        .unwrap();
    fs::write(world.home.join("Documents/Set/movie.bin"), b"edited").unwrap();
    let report = world.run(&preview.preview_id, &world.trash()).unwrap();
    assert!(matches!(
        report.results[0].outcome,
        FfiItemOutcome::Skipped {
            reason: FfiSkipReason::Changed,
            ..
        }
    ));
    assert!(archive.exists());
}
