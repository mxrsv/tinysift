//! Freed bytes measured around real permanent deletions (AC-H3): the
//! deletion goes on past an entry it cannot remove, and what it freed is the
//! item's freed bytes before minus after, hard links and clones included.
//! Only temp-dir fixtures are deleted.

use super::removal::{allocated, planned, Sinks};
use super::*;
use crate::clean::failure_cause::FailureCause;
use crate::clean::fs_safe::{dir_size_checked, SafeDelete};

/// Clears `chflags` on drop, so a failed assert never leaves a locked file.
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
    use std::os::unix::ffi::OsStrExt;
    let encoded = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
    // SAFETY: a NUL-terminated path inside the test-owned temp tree.
    let rc = unsafe { libc::chflags(encoded.as_ptr(), flags as _) };
    assert_eq!(rc, 0, "chflags: {}", io::Error::last_os_error());
}

fn run_real(plan: &[PlannedItem]) -> CheckedExecReport {
    let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(fixture("user-logs"))];
    let home = std::env::temp_dir();
    let store = TempStore::new();
    let lease = store.store.lease().unwrap();
    let sinks = Sinks::default();
    let ctx = ExecContext {
        providers: &providers,
        home: Some(&home),
        probe: &MockChecker::none(),
        trash: &sinks,
        delete: &SafeDelete,
        elevated: &NeverDelete,
        cancel: None,
        progress: None,
        policy: &lease,
    };
    execute_checked(plan, &ctx)
}

#[test]
fn a_locked_file_leaves_the_item_partly_removed_with_its_bytes_measured() {
    let root = fixture_root("partly", &[]);
    let item = root.join("cache");
    fs::create_dir_all(item.join("sub")).unwrap();
    fs::write(item.join("a"), vec![1u8; 20_000]).unwrap();
    fs::write(item.join("sub/b"), vec![2u8; 10_000]).unwrap();
    fs::write(item.join("sub/locked"), vec![3u8; 30_000]).unwrap();
    let freed = allocated(&item.join("a")) + allocated(&item.join("sub/b"));
    let left = allocated(&item.join("sub/locked"));
    let lock = Locked::new(item.join("sub/locked"));

    let report = run_real(&[planned("user-logs", &root, "cache", Removal::Delete)]);

    let result = &report.results[0];
    assert_eq!(result.extent, RemovalExtent::PartlyRemoved, "{result:?}");
    assert!(
        matches!(&result.outcome, ItemOutcome::DeleteFailed { detail, source_present: true }
            if detail.contains("sub/locked")),
        "{:?}",
        result.outcome
    );
    assert_eq!((result.bytes_freed, result.bytes_left), (freed, left));
    let failure = result.failure.as_ref().unwrap();
    assert_eq!(failure.cause, FailureCause::Other, "a lock the user set");
    assert!(failure.raw_error.contains("sub/locked"), "{failure:?}");
    assert_eq!(report.bytes_freed, freed);
    assert_eq!(report.bytes_left(), left);
    assert_eq!(report.extent_count(RemovalExtent::PartlyRemoved), 1);
    assert!(!item.join("a").exists() && !item.join("sub/b").exists());
    assert!(item.join("sub/locked").exists(), "the locked file stays");
    drop(lock);
    let _ = remove_recursive_safe(&root);
}

#[test]
fn freed_is_before_minus_after_for_hard_links() {
    let root = fixture_root("links", &[]);
    let item = root.join("cache");
    fs::create_dir(&item).unwrap();
    fs::write(item.join("own"), vec![1u8; 20_000]).unwrap();
    fs::write(item.join("shared"), vec![2u8; 50_000]).unwrap();
    fs::hard_link(item.join("shared"), root.join("outside")).unwrap();
    fs::write(item.join("pair"), vec![3u8; 40_000]).unwrap();
    fs::hard_link(item.join("pair"), item.join("pair-locked")).unwrap();
    let before = dir_size_checked(&item, &ScanContext::unchecked());
    let own = allocated(&item.join("own"));
    let pair = allocated(&item.join("pair"));
    assert_eq!(before, own + pair, "the outside link keeps `shared`");
    let lock = Locked::new(item.join("pair-locked"));

    let report = run_real(&[planned("user-logs", &root, "cache", Removal::Delete)]);

    let result = &report.results[0];
    let after = dir_size_checked(&item, &ScanContext::unchecked());
    assert_eq!(after, pair, "the locked link alone now holds the data");
    assert_eq!(result.bytes_freed, before - after);
    assert_eq!(
        result.bytes_freed, own,
        "unlinking one of two links frees nothing"
    );
    assert_eq!(result.bytes_left, pair);
    assert_eq!(result.extent, RemovalExtent::PartlyRemoved);
    assert_eq!(fs::read(root.join("outside")).unwrap().len(), 50_000);
    drop(lock);
    let _ = remove_recursive_safe(&root);
}

#[cfg(target_os = "macos")]
#[test]
fn freed_is_before_minus_after_for_clones() {
    use std::os::unix::ffi::OsStrExt;
    let clone = |from: &Path, to: &Path| {
        let from = std::ffi::CString::new(from.as_os_str().as_bytes()).unwrap();
        let to = std::ffi::CString::new(to.as_os_str().as_bytes()).unwrap();
        // SAFETY: both arguments are valid NUL-terminated paths.
        let rc = unsafe { libc::clonefile(from.as_ptr(), to.as_ptr(), 0) };
        assert_eq!(rc, 0, "clonefile: {}", io::Error::last_os_error());
    };
    let root = fixture_root("clones", &[]);
    let item = root.join("cache");
    fs::create_dir(&item).unwrap();
    fs::write(item.join("original"), vec![7u8; 300_000]).unwrap();
    clone(&item.join("original"), &item.join("copy"));
    fs::write(item.join("kept"), vec![8u8; 300_000]).unwrap();
    clone(&item.join("kept"), &root.join("outside"));
    let before = dir_size_checked(&item, &ScanContext::unchecked());
    let family = allocated(&item.join("original"));
    assert_eq!(
        before, family,
        "one family inside counts once; `kept` not at all"
    );

    let report = run_real(&[planned("user-logs", &root, "cache", Removal::Delete)]);

    let result = &report.results[0];
    assert_eq!(result.extent, RemovalExtent::Removed);
    assert_eq!(result.bytes_freed, before, "before minus nothing left");
    assert!(!result.freed_is_upper_bound, "APFS answered");
    assert_eq!(fs::read(root.join("outside")).unwrap().len(), 300_000);
    let _ = remove_recursive_safe(&root);
}

#[test]
fn an_unreadable_folder_is_no_permission_and_the_rest_still_goes() {
    use std::os::unix::fs::PermissionsExt;
    if unsafe { libc::geteuid() } == 0 {
        return; // root reads any folder
    }
    let root = fixture_root("denied", &[]);
    let item = root.join("cache");
    fs::create_dir_all(item.join("closed")).unwrap();
    fs::write(item.join("closed/inside"), vec![1u8; 10_000]).unwrap();
    fs::write(item.join("open"), vec![2u8; 10_000]).unwrap();
    let freed = allocated(&item.join("open"));
    fs::set_permissions(item.join("closed"), fs::Permissions::from_mode(0o000)).unwrap();
    let report = run_real(&[planned("user-logs", &root, "cache", Removal::Delete)]);
    fs::set_permissions(item.join("closed"), fs::Permissions::from_mode(0o700)).unwrap();

    let result = &report.results[0];
    assert_eq!(result.extent, RemovalExtent::PartlyRemoved);
    assert_eq!(result.bytes_freed, freed);
    assert_eq!(
        result.failure.as_ref().map(|f| f.cause),
        Some(FailureCause::NoPermission)
    );
    assert!(!item.join("open").exists());
    let _ = remove_recursive_safe(&root);
}

/// Runs `swap` once, right after the item's measuring walk.
fn after_measure(swap: impl FnMut(&Path) + 'static) {
    super::super::measure::AFTER_MEASURE.with(|hook| *hook.borrow_mut() = Some(Box::new(swap)));
}

fn clear_hook() {
    super::super::measure::AFTER_MEASURE.with(|hook| *hook.borrow_mut() = None);
}

#[test]
fn a_root_replaced_during_the_measuring_walk_is_never_deleted() {
    let root = fixture_root("swap-walk", &[]);
    let item = root.join("cache");
    fs::create_dir(&item).unwrap();
    fs::write(item.join("a"), b"selected").unwrap();
    let plan = [planned("user-logs", &root, "cache", Removal::Delete)];
    let retired = root.join("retired");
    after_measure(move |path| {
        fs::rename(path, &retired).unwrap();
        fs::create_dir(path).unwrap();
        fs::write(path.join("precious"), b"keep").unwrap();
    });
    let report = run_real(&plan);
    clear_hook();

    let result = &report.results[0];
    assert_eq!(result.outcome, ItemOutcome::Skipped(SkipReason::Changed));
    assert_eq!(result.extent, RemovalExtent::NotRemoved);
    assert_eq!(
        result.failure.as_ref().map(|f| f.cause),
        Some(FailureCause::ChangedSinceScan)
    );
    assert_eq!(fs::read(item.join("precious")).unwrap(), b"keep");
    assert_eq!(fs::read(root.join("retired/a")).unwrap(), b"selected");
    let _ = remove_recursive_safe(&root);
}

/// Swaps the root after every check, right before the deletion starts.
struct SwapThenDelete;

impl Delete for SwapThenDelete {
    fn delete(&self, path: &Path) -> io::Result<()> {
        panic!("{} must be deleted best effort", path.display());
    }

    fn delete_each(
        &self,
        path: &Path,
        identity: Option<crate::clean::fs_safe::FileIdentity>,
    ) -> crate::clean::fs_safe::TreeRemoval {
        fs::rename(path, path.with_file_name("retired")).unwrap();
        fs::create_dir(path).unwrap();
        fs::write(path.join("precious"), b"keep").unwrap();
        SafeDelete.delete_each(path, identity)
    }
}

#[test]
fn the_deletion_refuses_a_root_that_is_not_the_scanned_file() {
    let root = fixture_root("swap-pin", &[]);
    fs::create_dir(root.join("cache")).unwrap();
    fs::write(root.join("cache/a"), b"selected").unwrap();
    let plan = [planned("user-logs", &root, "cache", Removal::Delete)];
    let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(fixture("user-logs"))];
    let home = std::env::temp_dir();
    let store = TempStore::new();
    let lease = store.store.lease().unwrap();
    let sinks = Sinks::default();
    let ctx = ExecContext {
        providers: &providers,
        home: Some(&home),
        probe: &MockChecker::none(),
        trash: &sinks,
        delete: &SwapThenDelete,
        elevated: &NeverDelete,
        cancel: None,
        progress: None,
        policy: &lease,
    };
    let report = execute_checked(&plan, &ctx);

    let result = &report.results[0];
    assert!(matches!(result.outcome, ItemOutcome::DeleteFailed { .. }));
    assert_eq!(result.extent, RemovalExtent::NotRemoved);
    assert_eq!(
        result.failure.as_ref().map(|f| f.cause),
        Some(FailureCause::ChangedSinceScan)
    );
    assert_eq!(fs::read(root.join("cache/precious")).unwrap(), b"keep");
    assert_eq!(fs::read(root.join("retired/a")).unwrap(), b"selected");
    let _ = remove_recursive_safe(&root);
}
