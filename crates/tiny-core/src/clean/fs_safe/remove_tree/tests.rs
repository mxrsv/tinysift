use super::*;
use std::fs;
use std::os::unix::fs::symlink;
use std::os::unix::fs::PermissionsExt;

fn fixture() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let base = fs::canonicalize(temp.path()).unwrap();
    fs::create_dir_all(base.join("item/sub")).unwrap();
    fs::write(base.join("item/sub/file"), b"selected").unwrap();
    fs::create_dir(base.join("outside")).unwrap();
    fs::write(base.join("outside/precious"), b"keep").unwrap();
    (temp, base)
}

#[test]
fn a_pre_open_directory_swap_never_enters_the_symlink_target() {
    let (_temp, base) = fixture();
    let sub = base.join("item/sub");
    let result = remove_observed(&base.join("item"), true, &mut |stage, path| {
        if matches!(stage, Stage::BeforeDirectoryOpen) && path == sub {
            fs::rename(&sub, base.join("retired")).unwrap();
            symlink(base.join("outside"), &sub).unwrap();
        }
    });
    assert!(result.is_err());
    assert_eq!(fs::read(base.join("outside/precious")).unwrap(), b"keep");
    assert_eq!(fs::read(base.join("retired/file")).unwrap(), b"selected");
    assert!(fs::symlink_metadata(sub).unwrap().is_symlink());
}

#[test]
fn a_post_open_swap_stays_on_the_pinned_directory_and_refuses_the_replacement() {
    let (_temp, base) = fixture();
    let sub = base.join("item/sub");
    let result = remove_observed(&base.join("item"), true, &mut |stage, path| {
        if matches!(stage, Stage::AfterDirectoryOpen) && path == sub {
            fs::rename(&sub, base.join("retired")).unwrap();
            symlink(base.join("outside"), &sub).unwrap();
        }
    });
    assert!(result.unwrap_err().to_string().contains("changed"));
    assert_eq!(fs::read(base.join("outside/precious")).unwrap(), b"keep");
    assert!(
        !base.join("retired/file").exists(),
        "the opened inode is traversed"
    );
    assert!(fs::symlink_metadata(sub).unwrap().is_symlink());
}

#[test]
fn an_ancestor_swapped_before_open_is_not_followed() {
    let (_temp, base) = fixture();
    let ancestor = base.join("item");
    let result = remove_observed(&ancestor.join("sub"), true, &mut |stage, path| {
        if matches!(stage, Stage::BeforeAncestorOpen) && path == ancestor {
            fs::rename(&ancestor, base.join("retired")).unwrap();
            symlink(base.join("outside"), &ancestor).unwrap();
        }
    });
    assert!(result.is_err());
    assert_eq!(fs::read(base.join("outside/precious")).unwrap(), b"keep");
    assert_eq!(
        fs::read(base.join("retired/sub/file")).unwrap(),
        b"selected"
    );
}

#[test]
fn a_real_directory_replacement_before_open_is_refused() {
    let (_temp, base) = fixture();
    let sub = base.join("item/sub");
    let result = remove_observed(&base.join("item"), true, &mut |stage, path| {
        if matches!(stage, Stage::BeforeDirectoryOpen) && path == sub {
            fs::rename(&sub, base.join("retired")).unwrap();
            fs::rename(base.join("outside"), &sub).unwrap();
        }
    });
    assert!(result.unwrap_err().to_string().contains("changed"));
    assert_eq!(fs::read(sub.join("precious")).unwrap(), b"keep");
}

#[test]
fn permanent_deletion_never_enters_a_directory_on_another_device() {
    let (_temp, base) = fixture();
    let parent = open_directory(
        libc::AT_FDCWD,
        &name(base.as_os_str()).unwrap(),
        SEARCH_ACCESS,
    )
    .unwrap();
    let leaf = name(OsStr::new("item")).unwrap();
    let expected = stat_at(parent.as_raw_fd(), &leaf).unwrap();
    // A mismatching device stands in for a mounted child, without a real mount.
    let result = remove_entry(
        parent.as_raw_fd(),
        &leaf,
        &expected,
        Some(expected.st_dev.wrapping_add(1)),
        &base.join("item"),
        &mut |_, _| {},
        &mut Sink {
            best_effort: false,
            removal: TreeRemoval::default(),
            freed: FreedBytes::default(),
        },
    );
    assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::EXDEV));
    assert_eq!(fs::read(base.join("item/sub/file")).unwrap(), b"selected");
}

#[test]
fn normal_trees_leaf_links_files_and_missing_paths() {
    let (_temp, base) = fixture();
    symlink(base.join("outside"), base.join("item/link")).unwrap();
    let raw_name = OsStr::from_bytes(b"not-utf8-\xff");
    match fs::write(base.join("item").join(raw_name), b"raw") {
        Ok(()) => {}
        // APFS rejects non-UTF-8 names; exercise the byte-preserving boundary
        // below and exercise the actual entry on filesystems that allow it.
        Err(error) if error.raw_os_error() == Some(libc::EILSEQ) => {}
        Err(error) => panic!("cannot create test entry: {error}"),
    }
    assert_eq!(name(raw_name).unwrap().to_bytes(), raw_name.as_bytes());
    remove(&base.join("item"), true).unwrap();
    assert!(!base.join("item").exists());
    assert_eq!(fs::read(base.join("outside/precious")).unwrap(), b"keep");
    let link = base.join("link");
    symlink(base.join("outside"), &link).unwrap();
    remove(&link, true).unwrap();
    assert!(fs::symlink_metadata(link).is_err());
    fs::write(base.join("file"), b"file").unwrap();
    remove(&base.join("file"), false).unwrap();
    remove(&base.join("missing"), true).unwrap();
    remove(&base.join("absent/child"), true).unwrap();
    assert!(remove(Path::new("/"), true).is_err());
    assert!(remove(&base.join("item/../outside"), true).is_err());
    assert!(remove(&base.join(OsStr::from_bytes(b"bad\0name")), true).is_err());
}

#[test]
fn a_replacement_before_unlink_is_refused() {
    let (_temp, base) = fixture();
    let file = base.join("item/sub/file");
    let result = remove_observed(&base.join("item"), true, &mut |stage, path| {
        if matches!(stage, Stage::BeforeUnlink) && path == file {
            fs::rename(&file, base.join("retired")).unwrap();
            fs::create_dir(&file).unwrap();
            fs::write(file.join("precious"), b"keep").unwrap();
        }
    });
    assert!(result.unwrap_err().to_string().contains("changed"));
    assert_eq!(fs::read(file.join("precious")).unwrap(), b"keep");
}

#[test]
fn open_and_unlink_errors_propagate() {
    let (_temp, base) = fixture();
    assert_eq!(
        remove(&base.join("outside/precious/child"), true)
            .unwrap_err()
            .raw_os_error(),
        Some(libc::ENOTDIR)
    );
    let item = base.join("item");
    let result = remove_observed(&item, true, &mut |stage, path| {
        if matches!(stage, Stage::BeforeUnlink) && path == item {
            fs::write(item.join("arrived-late"), b"keep").unwrap();
        }
    });
    assert_eq!(result.unwrap_err().raw_os_error(), Some(libc::ENOTEMPTY));
    assert_eq!(fs::read(item.join("arrived-late")).unwrap(), b"keep");
}

#[test]
fn special_files_are_refused_instead_of_opened_or_silently_counted_as_deleted() {
    let (_temp, base) = fixture();
    let fifo = base.join("item/pipe");
    let encoded = name(fifo.as_os_str()).unwrap();
    // SAFETY: this path is inside the test-owned temp tree; mode is owner-only.
    assert_eq!(unsafe { libc::mkfifo(encoded.as_ptr(), 0o600) }, 0);
    assert_eq!(
        remove(&fifo, true).unwrap_err().kind(),
        io::ErrorKind::InvalidInput
    );
    assert!(fs::symlink_metadata(fifo).is_ok());
}

#[test]
fn ancestors_need_search_permission_but_not_read_permission() {
    let (_temp, base) = fixture();
    let parent = base.join("item");
    fs::set_permissions(parent.join("sub"), fs::Permissions::from_mode(0o700)).unwrap();
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o300)).unwrap();
    let result = remove(&parent.join("sub"), true);
    // Restore access before assertions so a failure still lets TempDir clean up.
    fs::set_permissions(&parent, fs::Permissions::from_mode(0o700)).unwrap();
    assert!(
        result.is_ok(),
        "a searchable ancestor needs no read permission: {result:?}"
    );
    assert!(!parent.join("sub").exists());
    assert_eq!(fs::read(base.join("outside/precious")).unwrap(), b"keep");
}

#[test]
fn enumerated_directories_still_need_read_permission() {
    let (_temp, base) = fixture();
    let item = base.join("item");
    fs::set_permissions(&item, fs::Permissions::from_mode(0o300)).unwrap();
    let result = remove(&item, true);
    fs::set_permissions(&item, fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(result.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
    assert_eq!(fs::read(item.join("sub/file")).unwrap(), b"selected");
}

/// Clears `chflags` on drop, so a failed assert never leaves a locked file
/// in the temp folder.
struct Locked(PathBuf);

impl Locked {
    fn new(path: PathBuf) -> Self {
        set_flags(&path, libc::UF_IMMUTABLE);
        Self(path)
    }
}

impl Drop for Locked {
    fn drop(&mut self) {
        set_flags(&self.0, 0);
    }
}

fn set_flags(path: &Path, flags: u32) {
    let encoded = name(path.as_os_str()).unwrap();
    // SAFETY: a NUL-terminated path inside the test-owned temp tree.
    let rc = unsafe { libc::chflags(encoded.as_ptr(), flags as _) };
    assert_eq!(rc, 0, "chflags: {}", io::Error::last_os_error());
}

#[test]
fn best_effort_removes_the_rest_of_the_item_past_a_locked_file() {
    let (_temp, base) = fixture();
    let item = base.join("item");
    fs::create_dir(item.join("other")).unwrap();
    fs::write(item.join("other/a"), b"a").unwrap();
    fs::write(item.join("b"), b"b").unwrap();
    let _lock = Locked::new(item.join("sub/file"));

    let removal = remove_each(&item, true, None);

    assert_eq!(removal.failed_entries, 1, "{removal:?}");
    assert_eq!(removal.failures[0].path, item.join("sub/file"));
    assert_eq!(removal.failures[0].error.raw_os_error(), Some(libc::EPERM));
    assert!(!removal.is_complete());
    assert_eq!(removal.removed_entries, 3, "other/a, other and b");
    assert!(!item.join("other").exists() && !item.join("b").exists());
    assert_eq!(fs::read(item.join("sub/file")).unwrap(), b"selected");
    assert_eq!(fs::read(base.join("outside/precious")).unwrap(), b"keep");
}

#[test]
fn best_effort_records_an_unreadable_folder_and_removes_its_siblings() {
    if unsafe { libc::geteuid() } == 0 {
        return; // root reads any folder
    }
    let (_temp, base) = fixture();
    let item = base.join("item");
    fs::write(item.join("b"), b"b").unwrap();
    fs::set_permissions(item.join("sub"), fs::Permissions::from_mode(0o000)).unwrap();
    let removal = remove_each(&item, true, None);
    fs::set_permissions(item.join("sub"), fs::Permissions::from_mode(0o700)).unwrap();

    assert_eq!(removal.failed_entries, 1, "{removal:?}");
    assert_eq!(
        removal.failures[0].error.kind(),
        io::ErrorKind::PermissionDenied
    );
    assert!(!item.join("b").exists());
    assert_eq!(fs::read(item.join("sub/file")).unwrap(), b"selected");
}

#[test]
fn best_effort_keeps_every_guard_for_the_entry_that_fails() {
    let (_temp, base) = fixture();
    let item = base.join("item");
    let sub = item.join("sub");
    fs::write(item.join("b"), b"b").unwrap();
    let removal = remove_each_observed(&item, true, None, &mut |stage, path| {
        if matches!(stage, Stage::AfterDirectoryOpen) && path == sub {
            fs::rename(&sub, base.join("retired")).unwrap();
            symlink(base.join("outside"), &sub).unwrap();
        }
    });
    assert_eq!(removal.failed_entries, 1, "{removal:?}");
    assert!(is_entry_changed(&removal.failures[0].error));
    assert!(fs::symlink_metadata(&sub).unwrap().is_symlink());
    assert_eq!(fs::read(base.join("outside/precious")).unwrap(), b"keep");
    assert!(!item.join("b").exists(), "the sibling still goes");
}

#[test]
fn best_effort_reports_entries_that_arrived_and_failures_at_the_root() {
    let (_temp, base) = fixture();
    let item = base.join("item");
    let removal = remove_each_observed(&item, true, None, &mut |stage, path| {
        if matches!(stage, Stage::BeforeUnlink) && path == item {
            fs::write(item.join("arrived-late"), b"keep").unwrap();
        }
    });
    assert_eq!(removal.failed_entries, 1);
    assert_eq!(
        removal.failures[0].error.raw_os_error(),
        Some(libc::ENOTEMPTY)
    );
    assert_eq!(fs::read(item.join("arrived-late")).unwrap(), b"keep");

    assert!(remove_each(&base.join("missing"), true, None).is_complete());
    let root = remove_each(Path::new("/"), true, None);
    assert_eq!((root.failed_entries, root.removed_entries), (1, 0));
    let changed = remove_each_observed(&item, true, None, &mut |stage, path| {
        if matches!(stage, Stage::BeforeDirectoryOpen) && path == item {
            fs::rename(&item, base.join("retired")).unwrap();
            fs::create_dir(&item).unwrap();
        }
    });
    assert!(is_entry_changed(&changed.failures[0].error));
    assert_eq!(changed.removed_entries, 0, "a replaced root is left whole");
}

#[test]
fn best_effort_starts_only_from_the_pinned_root() {
    let (_temp, base) = fixture();
    let item = base.join("item");
    let meta = fs::symlink_metadata(&item).unwrap();
    let pinned = FileIdentity {
        device: std::os::unix::fs::MetadataExt::dev(&meta),
        inode: std::os::unix::fs::MetadataExt::ino(&meta),
    };
    let other = FileIdentity {
        inode: pinned.inode + 1,
        ..pinned
    };
    let refused = remove_each(&item, true, Some(other));
    assert!(is_entry_changed(&refused.failures[0].error));
    assert_eq!(refused.removed_entries, 0);
    assert_eq!(fs::read(item.join("sub/file")).unwrap(), b"selected");
    assert!(remove_each(&item, true, Some(pinned)).is_complete());
    assert!(!item.exists());
}

#[test]
fn a_child_gone_before_its_open_is_removed_and_one_turned_into_a_link_is_changed() {
    let (_temp, base) = fixture();
    let item = base.join("item");
    let sub = item.join("sub");
    let removal = remove_each_observed(&item, true, None, &mut |stage, path| {
        if matches!(stage, Stage::BeforeDirectoryOpen) && path == sub {
            fs::remove_file(sub.join("file")).unwrap();
            fs::remove_dir(&sub).unwrap();
        }
    });
    assert!(removal.is_complete(), "{removal:?}");
    assert!(!item.exists());

    let (_temp, base) = fixture();
    let item = base.join("item");
    let sub = item.join("sub");
    let removal = remove_each_observed(&item, true, None, &mut |stage, path| {
        if matches!(stage, Stage::BeforeDirectoryOpen) && path == sub {
            fs::rename(&sub, base.join("retired")).unwrap();
            symlink(base.join("outside"), &sub).unwrap();
        }
    });
    assert_eq!(removal.failed_entries, 1);
    assert!(is_entry_changed(&removal.failures[0].error));
    assert_eq!(fs::read(base.join("outside/precious")).unwrap(), b"keep");
}

fn allocated(path: &Path) -> u64 {
    std::os::unix::fs::MetadataExt::blocks(&fs::symlink_metadata(path).unwrap()) * 512
}

#[test]
fn best_effort_counts_what_it_unlinked_by_the_freed_size_rule() {
    let (_temp, base) = fixture();
    let item = base.join("item");
    fs::write(item.join("plain"), vec![1u8; 20_000]).unwrap();
    fs::write(item.join("pair"), vec![2u8; 30_000]).unwrap();
    fs::hard_link(item.join("pair"), item.join("sub/pair-link")).unwrap();
    fs::write(item.join("shared"), vec![3u8; 40_000]).unwrap();
    fs::hard_link(item.join("shared"), base.join("outside/shared")).unwrap();
    let expected = allocated(&item.join("sub/file"))
        + allocated(&item.join("plain"))
        + allocated(&item.join("pair"));

    let removal = remove_each(&item, true, None);

    assert!(removal.is_complete(), "{removal:?}");
    assert_eq!(
        removal.freed, expected,
        "the pair once; `shared` keeps its outside link"
    );
    assert_eq!(removal.freed_is_upper_bound, cfg!(not(target_os = "macos")));
    assert_eq!(fs::read(base.join("outside/shared")).unwrap().len(), 40_000);
}

#[test]
fn best_effort_counts_only_the_files_it_removed() {
    let (_temp, base) = fixture();
    let item = base.join("item");
    fs::write(item.join("plain"), vec![1u8; 20_000]).unwrap();
    fs::write(item.join("locked"), vec![2u8; 20_000]).unwrap();
    let expected = allocated(&item.join("sub/file")) + allocated(&item.join("plain"));
    let _lock = Locked::new(item.join("locked"));

    let removal = remove_each(&item, true, None);

    assert_eq!(removal.failed_entries, 1);
    assert_eq!(removal.freed, expected, "the locked file stays, uncounted");
    assert!(item.join("locked").exists());
}

#[cfg(target_os = "macos")]
#[test]
fn best_effort_counts_a_clone_family_once_and_not_one_with_a_member_outside() {
    let clone = |from: &Path, to: &Path| {
        let from = name(from.as_os_str()).unwrap();
        let to = name(to.as_os_str()).unwrap();
        // SAFETY: both arguments are valid NUL-terminated paths.
        let rc = unsafe { libc::clonefile(from.as_ptr(), to.as_ptr(), 0) };
        assert_eq!(rc, 0, "clonefile: {}", io::Error::last_os_error());
    };
    let (_temp, base) = fixture();
    let item = base.join("item");
    fs::write(item.join("original"), vec![7u8; 300_000]).unwrap();
    clone(&item.join("original"), &item.join("copy"));
    fs::write(item.join("kept"), vec![8u8; 300_000]).unwrap();
    clone(&item.join("kept"), &base.join("outside/kept"));
    let expected = allocated(&item.join("sub/file")) + allocated(&item.join("original"));

    let removal = remove_each(&item, true, None);

    assert!(removal.is_complete());
    assert_eq!(removal.freed, expected);
    assert!(!removal.freed_is_upper_bound, "APFS answered");
}
