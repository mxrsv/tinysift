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
