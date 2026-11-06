use super::*;
use std::fs;
use std::path::PathBuf;

fn os(code: i32) -> io::Error {
    io::Error::from_raw_os_error(code)
}

fn own_file() -> (tempfile::TempDir, PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let file = fs::canonicalize(temp.path()).unwrap().join("file");
    fs::write(&file, b"x").unwrap();
    (temp, file)
}

#[test]
fn each_errno_maps_to_its_cause() {
    let (_temp, file) = own_file();
    let cases = [
        (libc::ENOENT, FailureCause::AlreadyGone),
        (libc::EBUSY, FailureCause::InUse),
        (libc::ETXTBSY, FailureCause::InUse),
        (libc::EROFS, FailureCause::ProtectedByMacOs),
        (libc::ENOTEMPTY, FailureCause::ChangedSinceScan),
        (libc::EXDEV, FailureCause::Other),
        (libc::EIO, FailureCause::Other),
    ];
    for (code, cause) in cases {
        assert_eq!(cause_of_io(&file, &os(code)), cause, "errno {code}");
    }
    assert_eq!(
        cause_of_io(&file, &io::Error::other("no errno")),
        FailureCause::Other
    );
    assert_eq!(
        cause_of_io(&file, &crate::clean::fs_safe::entry_changed()),
        FailureCause::ChangedSinceScan
    );
}

#[test]
fn a_permission_error_on_my_own_file_is_no_permission() {
    if unsafe { libc::geteuid() } == 0 {
        return; // root owns nothing that could need an administrator
    }
    let (_temp, file) = own_file();
    assert_eq!(
        cause_of_io(&file, &os(libc::EPERM)),
        FailureCause::NoPermission
    );
    assert_eq!(
        cause_of_io(&file, &os(libc::EACCES)),
        FailureCause::NoPermission
    );
    assert_eq!(
        cause_of_io(&file.with_file_name("gone"), &os(libc::EPERM)),
        FailureCause::NoPermission,
        "nothing left to look at"
    );
}

#[cfg(target_os = "macos")]
#[test]
fn flags_and_owners_decide_a_permission_error() {
    if unsafe { libc::geteuid() } == 0 {
        return;
    }
    // Read-only looks: SIP's `restricted` on /System, root's /private/etc.
    assert_eq!(
        cause_of_io(Path::new("/System"), &os(libc::EPERM)),
        FailureCause::ProtectedByMacOs
    );
    assert_eq!(
        cause_of_io(Path::new("/private/etc/hosts"), &os(libc::EACCES)),
        FailureCause::NeedsAdministrator
    );
    let (_temp, file) = own_file();
    let encoded = std::ffi::CString::new(file.to_str().unwrap()).unwrap();
    let set = |flags: u32| {
        // SAFETY: a NUL-terminated path in the test's own temp folder.
        assert_eq!(unsafe { libc::chflags(encoded.as_ptr(), flags as _) }, 0);
    };
    set(libc::UF_IMMUTABLE);
    let locked = cause_of_io(&file, &os(libc::EPERM));
    set(0);
    assert_eq!(locked, FailureCause::Other, "a lock the user set");
}

#[test]
fn skip_reasons_map_to_causes_and_kept_paths_are_no_failure() {
    use crate::clean::checked_execute::SkipReason as S;
    let cause = |reason: S| skip_failure(&reason).map(|f| f.cause);
    assert_eq!(cause(S::Missing), Some(FailureCause::AlreadyGone));
    for changed in [S::Changed, S::Symlink, S::OutsideRoots] {
        assert_eq!(cause(changed), Some(FailureCause::ChangedSinceScan));
    }
    assert_eq!(
        cause(S::AppRunning("Xcode".into())),
        Some(FailureCause::InUse)
    );
    assert_eq!(
        cause(S::ProtectedPath(INSIDE_SYSTEM.into())),
        Some(FailureCause::ProtectedByMacOs)
    );
    assert_eq!(
        cause(S::ProtectedPath("the home folder".into())),
        Some(FailureCause::Other)
    );
    assert_eq!(cause(S::ReportOnly), Some(FailureCause::Other));
    assert_eq!(
        cause(S::KeepUncertain("?".into())),
        Some(FailureCause::Other)
    );
    let rule_path = PathBuf::from("/kept");
    assert_eq!(
        cause(S::Kept {
            rule_path: rule_path.clone()
        }),
        None
    );
    assert_eq!(cause(S::HoldsKept { rule_path }), None);
    let raw = skip_failure(&S::AppRunning("Xcode".into()))
        .unwrap()
        .raw_error;
    assert!(raw.contains("Xcode"), "{raw}");
}

#[test]
fn a_denied_trash_route_is_no_permission_with_its_raw_text() {
    let denied = trash_failure(&TrashError::AutomationDenied("(-1743)".into()));
    assert_eq!(denied.cause, FailureCause::NoPermission);
    assert!(denied.raw_error.contains("-1743"));
    let failed = trash_failure(&TrashError::Failed("Finder error".into()));
    assert_eq!(failed.cause, FailureCause::Other);
}
