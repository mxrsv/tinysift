use super::*;
use crate::clean::finder_trash::FinderTrashWith;
use crate::runner::test_support::MockRunner;
use std::fs;

fn watched() -> NativeTrash<FinderTrashWith<MockRunner>> {
    NativeTrash {
        empty: FinderTrashWith(MockRunner::new()),
    }
}

fn spawned(trash: &NativeTrash<FinderTrashWith<MockRunner>>) -> Vec<String> {
    let runner = &trash.empty.0;
    let mut calls = runner.output_calls.lock().unwrap().clone();
    calls.extend(runner.run_calls.lock().unwrap().iter().cloned());
    calls
}

#[test]
fn a_move_never_runs_osascript_and_reports_the_posix_error() {
    let temp = tempfile::tempdir().unwrap();
    let missing = fs::canonicalize(temp.path()).unwrap().join("missing");
    let trash = watched();
    // Refused before the native call: nothing to move.
    assert!(trash.move_to_trash(&missing).is_err());
    let target = temp.path().join("target");
    fs::create_dir(&target).unwrap();
    std::os::unix::fs::symlink(&target, temp.path().join("link")).unwrap();
    assert!(
        matches!(trash.move_to_trash(&temp.path().join("link")), Err(TrashError::Failed(m)) if m.contains("symlink"))
    );
    assert!(spawned(&trash).is_empty(), "{:?}", spawned(&trash));

    // Only Empty Trash goes to Finder.
    let _ = trash.empty_trash();
    let calls = spawned(&trash);
    assert_eq!(calls.len(), 1, "{calls:?}");
    assert!(calls[0].contains("osascript"), "{calls:?}");
}

#[cfg(target_os = "macos")]
#[test]
fn foundation_errors_carry_their_posix_code() {
    let temp = tempfile::tempdir().unwrap();
    let gone = fs::canonicalize(temp.path()).unwrap().join("gone");
    // trash_item itself, past the symlink check: Foundation answers.
    match trash_item(&gone) {
        Err(TrashError::Os { code, .. }) => assert_eq!(code, libc::ENOENT),
        other => panic!("not a POSIX error: {other:?}"),
    }
    assert_eq!(errno_of_cocoa(513), Some(libc::EACCES));
    assert_eq!(errno_of_cocoa(1), None);
}

/// Moves a temp file into the user's real Trash, then deletes it from
/// there. Run by hand: `cargo test -p tiny-core native_trash -- --ignored`.
#[cfg(target_os = "macos")]
#[test]
#[ignore = "moves a temp file through the user's Trash"]
fn a_move_lands_in_the_trash_without_finder() {
    let temp = tempfile::tempdir().unwrap();
    let file = fs::canonicalize(temp.path())
        .unwrap()
        .join("tinysift-native-trash-test");
    fs::write(&file, b"test").unwrap();
    let moved = trash_item(&file)
        .unwrap()
        .expect("Foundation names the location");
    assert!(fs::symlink_metadata(&file).is_err(), "moved away");
    assert_eq!(fs::read(&moved).unwrap(), b"test");
    assert!(
        moved.components().any(|c| c.as_os_str() == ".Trash"),
        "{moved:?}"
    );
    crate::clean::fs_safe::remove_recursive_safe(&moved).unwrap();
}
