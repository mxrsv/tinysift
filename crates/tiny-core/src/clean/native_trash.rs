//! Clean's moves to the Trash from TinySift's own process, through
//! `NSFileManager trashItemAtURL`: TinySift's Full Disk Access applies and
//! no Finder Automation permission is asked. Finder's Put Back does not
//! know such an item; it can still be dragged back out of the Trash.
//! Emptying the Trash stays with Finder ([`FinderTrash`]).

use std::path::Path;

use super::finder_trash::{refuse_symlink, FinderTrash, Trash, TrashError};

/// Moves natively; empties through `empty`, Finder by default.
pub struct NativeTrash<E: Trash = FinderTrash> {
    pub empty: E,
}

impl Default for NativeTrash {
    fn default() -> Self {
        Self { empty: FinderTrash }
    }
}

impl<E: Trash> Trash for NativeTrash<E> {
    fn move_to_trash(&self, path: &Path) -> Result<(), TrashError> {
        refuse_symlink(path)?;
        trash_item(path).map(|_| ())
    }

    fn empty_trash(&self) -> Result<(), TrashError> {
        self.empty.empty_trash()
    }
}

/// Moves `path` to the Trash of its volume and returns where it went, when
/// Foundation says. A move without a location still succeeded.
#[cfg(target_os = "macos")]
fn trash_item(path: &Path) -> Result<Option<std::path::PathBuf>, TrashError> {
    use objc2::rc::{autoreleasepool, Retained};
    use objc2_foundation::{NSFileManager, NSString, NSURL};

    let posix = path
        .to_str()
        .ok_or_else(|| TrashError::Failed(format!("non-UTF-8 path: {}", path.display())))?;
    autoreleasepool(|_| {
        let url = NSURL::fileURLWithPath(&NSString::from_str(posix));
        let mut moved: Option<Retained<NSURL>> = None;
        NSFileManager::defaultManager()
            .trashItemAtURL_resultingItemURL_error(&url, Some(&mut moved))
            .map_err(|error| ns_error(&error))?;
        Ok(moved
            .and_then(|url| url.path())
            .map(|path| std::path::PathBuf::from(path.to_string())))
    })
}

#[cfg(not(target_os = "macos"))]
fn trash_item(_: &Path) -> Result<Option<std::path::PathBuf>, TrashError> {
    Err(TrashError::Failed("no native Trash on this system".into()))
}

/// `TrashError::Os` with the POSIX code under the Cocoa error, or one that
/// matches its Cocoa code; `Failed` when there is none.
#[cfg(target_os = "macos")]
fn ns_error(error: &objc2_foundation::NSError) -> TrashError {
    use objc2_foundation::{NSError, NSPOSIXErrorDomain, NSUnderlyingErrorKey};
    // SAFETY: Foundation's constant error keys and domains, never mutated.
    let (key, posix) = unsafe { (NSUnderlyingErrorKey, NSPOSIXErrorDomain) };
    let underlying = error
        .userInfo()
        .objectForKey(key)
        .and_then(|object| object.downcast::<NSError>().ok())
        .filter(|inner| &*inner.domain() == posix)
        .and_then(|inner| i32::try_from(inner.code()).ok());
    let detail = format!(
        "{} ({} {})",
        error.localizedDescription(),
        error.domain(),
        error.code()
    );
    match underlying.or_else(|| errno_of_cocoa(error.code())) {
        Some(code) => TrashError::Os { code, detail },
        None => TrashError::Failed(detail),
    }
}

/// The POSIX code that matches a Cocoa file error code
/// (`NSFileNoSuchFileError`, `NSFileReadNoPermissionError`,
/// `NSFileWriteNoPermissionError`, `NSFileWriteVolumeReadOnlyError`).
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
fn errno_of_cocoa(code: isize) -> Option<i32> {
    match code {
        4 => Some(libc::ENOENT),
        257 | 513 => Some(libc::EACCES),
        642 => Some(libc::EROFS),
        _ => None,
    }
}

#[cfg(test)]
mod tests;
