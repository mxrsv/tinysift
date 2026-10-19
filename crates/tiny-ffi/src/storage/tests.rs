//! Storage adapter tests: temp dirs only, never real user folders.

use std::path::PathBuf;
use std::sync::Mutex;

use super::*;
use crate::FfiProgress;

#[derive(Default)]
struct Listener(Mutex<Vec<FfiProgress>>);

impl ProgressListener for Listener {
    fn on_progress(&self, progress: FfiProgress) {
        self.0.lock().unwrap().push(progress);
    }
}

fn fixture(name: &str) -> PathBuf {
    let root = std::fs::canonicalize(std::env::temp_dir())
        .unwrap()
        .join(format!("tiny-ffi-storage-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("sub")).unwrap();
    std::fs::write(root.join("a"), b"abc").unwrap();
    std::fs::write(root.join("sub/b"), b"12345").unwrap();
    root
}

#[test]
fn scan_converts_the_tree_and_reports_progress() {
    let dir = fixture("convert");
    let session = TinySession::new();
    let listener = Listener::default();
    let report = session
        .scan_storage(&dir, &CancellationToken::default(), &listener)
        .unwrap();
    assert_eq!(report.root.bytes, 8);
    assert_eq!(report.files_scanned, 2);
    assert_eq!(report.root.path, dir.display().to_string());
    let sub = report
        .root
        .children
        .iter()
        .find(|c| c.name == "sub")
        .unwrap();
    assert!(sub.is_directory);
    assert_eq!(sub.children[0].bytes, 5);
    assert!(!report.incomplete);
    assert_eq!(
        (report.depth_limited_count, report.other_volume_count),
        (0, 0)
    );
    assert!(!listener.0.lock().unwrap().is_empty());
    assert!(!session.is_busy(), "the gate is released");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn cancelled_scan_returns_cancelled_and_releases_the_gate() {
    let dir = fixture("cancel");
    let session = TinySession::new();
    let token = CancellationToken::default();
    token.cancel();
    let listener = Listener::default();
    let result = session.scan_storage(&dir, &token, &listener);
    assert!(matches!(result, Err(FfiError::Cancelled)));
    let updates = listener.0.lock().unwrap();
    assert_eq!(
        updates.len(),
        1,
        "cancelled work must not flood Swift callbacks"
    );
    assert_eq!((updates[0].completed, updates[0].total), (0, Some(0)));
    drop(updates);
    assert!(!session.is_busy());
    assert!(session
        .scan_storage(&dir, &CancellationToken::default(), &Listener::default())
        .is_ok());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn scan_is_rejected_while_another_operation_holds_the_gate() {
    let dir = fixture("busy");
    let session = TinySession::new();
    let _gate = session.begin().unwrap();
    let result = session.scan_storage(&dir, &CancellationToken::default(), &Listener::default());
    assert!(matches!(result, Err(FfiError::Busy)));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn invalid_roots_are_typed_errors_and_release_the_gate() {
    let dir = fixture("invalid");
    let session = TinySession::new();
    let token = CancellationToken::default();
    let listener = Listener::default();
    for root in [
        PathBuf::from("relative"),
        dir.join("a"),
        dir.join("a/../sub"),
    ] {
        assert!(
            matches!(
                session.scan_storage(&root, &token, &listener),
                Err(FfiError::InvalidInput { .. })
            ),
            "{}",
            root.display()
        );
    }
    let link = dir.join("link");
    std::os::unix::fs::symlink(dir.join("sub"), &link).unwrap();
    assert!(matches!(
        session.scan_storage(&link, &token, &listener),
        Err(FfiError::InvalidInput { .. })
    ));
    assert!(!session.is_busy());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn partial_trees_keep_their_coverage_flags() {
    use std::os::unix::fs::PermissionsExt;
    let dir = fixture("partial");
    let locked = dir.join("locked");
    std::fs::create_dir(&locked).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let report = TinySession::new()
        .scan_storage(&dir, &CancellationToken::default(), &Listener::default())
        .unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(report.incomplete && report.root.unreadable);
    assert_eq!(report.skipped_count, 1);
    assert!(report
        .root
        .children
        .iter()
        .any(|c| c.name == "locked" && c.unreadable));
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn disk_capacity_is_none_or_consistent() {
    if let Some(disk) = disk_capacity() {
        assert!(disk.total_bytes > 0 && disk.available_bytes <= disk.total_bytes);
    }
}

fn node(name: &str, seed: u64, children: Vec<SpaceNode>) -> SpaceNode {
    SpaceNode {
        name: name.into(),
        path: format!("/root/{name}"),
        bytes: seed,
        allocated_bytes: seed + 1,
        age_seconds: seed + 2,
        is_directory: seed.is_multiple_of(2),
        children,
        omitted_count: seed + 3,
        omitted_bytes: seed + 4,
        unreadable: seed.is_multiple_of(3),
        depth_limited: seed.is_multiple_of(5),
    }
}

fn expected(name: &str, seed: u64, children: Vec<FfiSpaceNode>) -> FfiSpaceNode {
    FfiSpaceNode {
        name: name.into(),
        path: format!("/root/{name}"),
        bytes: seed,
        allocated_bytes: seed + 1,
        age_seconds: seed + 2,
        is_directory: seed.is_multiple_of(2),
        children,
        omitted_count: seed + 3,
        omitted_bytes: seed + 4,
        unreadable: seed.is_multiple_of(3),
        depth_limited: seed.is_multiple_of(5),
    }
}

#[test]
fn every_report_and_node_field_is_converted_at_every_depth() {
    // Distinct values per field, so a swapped or dropped field cannot match by accident.
    let report = SpaceReport {
        root: node("a", 10, vec![node("b", 21, vec![node("c", 32, vec![])])]),
        files_scanned: 7,
        skipped_paths: vec!["/x".into(), "/y".into()],
        skipped_count: 9,
        depth_limited_count: 4,
        other_volume_count: 5,
        incomplete: true,
        cancelled: false,
    };
    let converted = FfiStorageReport::from(report);
    assert_eq!(
        converted,
        FfiStorageReport {
            root: expected(
                "a",
                10,
                vec![expected("b", 21, vec![expected("c", 32, vec![])])]
            ),
            files_scanned: 7,
            skipped_paths: vec!["/x".into(), "/y".into()],
            skipped_count: 9,
            depth_limited_count: 4,
            other_volume_count: 5,
            incomplete: true,
        }
    );
}

#[test]
fn disk_capacity_fields_are_converted() {
    let converted = FfiDiskCapacity::from(DiskCapacity {
        name: "Macintosh HD".into(),
        mount_point: "/System/Volumes/Data".into(),
        total_bytes: 500,
        available_bytes: 60,
    });
    assert_eq!(
        converted,
        FfiDiskCapacity {
            name: "Macintosh HD".into(),
            mount_point: "/System/Volumes/Data".into(),
            total_bytes: 500,
            available_bytes: 60,
        }
    );
}
