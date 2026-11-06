//! Why a planned item was not (wholly) removed, as one cause the app can
//! name with one sentence and one action. The raw error text stays beside
//! the cause for the run report; it is never the cause.

use std::io;
use std::path::Path;

use super::checked_execute::SkipReason;
use super::finder_trash::TrashError;

/// The one reason an item, or part of it, stayed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FailureCause {
    /// macOS refused TinySift (Full Disk Access, or a folder's own mode).
    NoPermission,
    /// An app is running, or the file is busy.
    InUse,
    /// Owned by another user or root: only an administrator can remove it.
    NeedsAdministrator,
    /// System Integrity Protection, a read-only or sealed volume, a system
    /// flag, or a path TinySift never touches (`/System`).
    ProtectedByMacOs,
    /// Replaced, edited or added to since the scan.
    ChangedSinceScan,
    /// Gone before the run reached it.
    AlreadyGone,
    Other,
}

/// A cause with the raw error it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Failure {
    pub cause: FailureCause,
    /// English diagnostic text (an `io::Error`, Finder's or a tool's
    /// message), for the run report only.
    pub raw_error: String,
}

impl Failure {
    pub fn new(cause: FailureCause, raw_error: impl Into<String>) -> Self {
        Self {
            cause,
            raw_error: raw_error.into(),
        }
    }
}

/// The reason `inside_system` gives for a path in `/System`.
pub(crate) const INSIDE_SYSTEM: &str = "inside /System";

/// The cause of `error`, raised acting on `path`. `EPERM` and `EACCES`
/// look at `path` again: a system flag means macOS protects it, a lock the
/// user set (Finder's Locked) is `Other`, another owner (of the entry or the
/// folder holding it) needs an administrator, and anything else is a
/// permission TinySift lacks (Full Disk Access).
pub fn cause_of_io(path: &Path, error: &io::Error) -> FailureCause {
    if super::fs_safe::is_entry_changed(error) {
        return FailureCause::ChangedSinceScan;
    }
    match error.raw_os_error() {
        Some(libc::ENOENT) => FailureCause::AlreadyGone,
        Some(libc::EBUSY | libc::ETXTBSY) => FailureCause::InUse,
        Some(libc::EROFS) => FailureCause::ProtectedByMacOs,
        // A folder that gained entries after it was listed.
        Some(libc::ENOTEMPTY) => FailureCause::ChangedSinceScan,
        Some(libc::EPERM | libc::EACCES) => permission_cause(path),
        _ => FailureCause::Other,
    }
}

fn permission_cause(path: &Path) -> FailureCause {
    use std::os::unix::fs::MetadataExt;
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return FailureCause::NoPermission;
    };
    match flags(&meta) {
        Flags::System => return FailureCause::ProtectedByMacOs,
        Flags::UserLock => return FailureCause::Other,
        Flags::None => {}
    }
    // SAFETY: geteuid has no preconditions and cannot fail.
    let me = unsafe { libc::geteuid() };
    let parent_owner = path
        .parent()
        .and_then(|parent| std::fs::symlink_metadata(parent).ok())
        .map(|parent| parent.uid());
    if meta.uid() != me || parent_owner.is_some_and(|uid| uid != me) {
        return FailureCause::NeedsAdministrator;
    }
    FailureCause::NoPermission
}

/// What `chflags` flags say about removing an entry.
enum Flags {
    /// SIP's `restricted`, or a system flag only root in single-user mode
    /// clears (`schg`, `sappnd`, `sunlnk`).
    System,
    /// `uchg` or `uappnd`, which the owner set (Finder's Locked).
    UserLock,
    None,
}

#[cfg(target_os = "macos")]
fn flags(meta: &std::fs::Metadata) -> Flags {
    use std::os::macos::fs::MetadataExt;
    // `<sys/stat.h>` values `libc` does not export.
    const SF_RESTRICTED: u32 = 0x0008_0000;
    const SF_NOUNLINK: u32 = 0x0010_0000;
    const SYSTEM: u32 = SF_RESTRICTED | SF_NOUNLINK | libc::SF_IMMUTABLE | libc::SF_APPEND;
    const USER: u32 = libc::UF_IMMUTABLE | libc::UF_APPEND;
    match meta.st_flags() {
        flags if flags & SYSTEM != 0 => Flags::System,
        flags if flags & USER != 0 => Flags::UserLock,
        _ => Flags::None,
    }
}

#[cfg(not(target_os = "macos"))]
fn flags(_: &std::fs::Metadata) -> Flags {
    Flags::None
}

/// The failure an item skipped for `reason` reports, or `None` when the
/// user's own saved rule kept it (a choice, not a failure).
pub fn skip_failure(reason: &SkipReason) -> Option<Failure> {
    let cause = match reason {
        SkipReason::Kept { .. } | SkipReason::HoldsKept { .. } => return None,
        SkipReason::Missing => FailureCause::AlreadyGone,
        SkipReason::Symlink | SkipReason::Changed | SkipReason::OutsideRoots => {
            FailureCause::ChangedSinceScan
        }
        SkipReason::AppRunning(_) => FailureCause::InUse,
        SkipReason::ProtectedPath(reason) if reason == INSIDE_SYSTEM => {
            FailureCause::ProtectedByMacOs
        }
        SkipReason::ReportOnly
        | SkipReason::UnknownCategory
        | SkipReason::ProtectedPath(_)
        | SkipReason::NotOnHomeVolume
        | SkipReason::ProviderGuard(_)
        | SkipReason::SafetyCheckFailed(_)
        | SkipReason::KeepUncertain(_) => FailureCause::Other,
    };
    Some(Failure::new(cause, format!("skipped: {reason:?}")))
}

/// The failure of a refused move to the Trash or Empty Trash.
pub fn trash_failure(error: &TrashError) -> Failure {
    let cause = match error {
        TrashError::AutomationDenied(_) => FailureCause::NoPermission,
        TrashError::Failed(_) => FailureCause::Other,
    };
    Failure::new(cause, error.to_string())
}

#[cfg(test)]
mod tests;
