//! Whether macOS treats a folder as one thing, a package (NL1): Finder shows
//! `Song.logicx` or `Deck.key` as a single file, so the walk must not list
//! what is inside it. Asked through `NSURLIsPackageKey`, which answers from
//! LaunchServices' type registry (an installed app's declared extensions) and
//! from the folder's own bundle bit. A folder with no extension is never
//! asked, so a bundle-bit package without an extension is walked: rare enough
//! that a lookup on every folder of a home is not worth it. The caller keeps
//! its own extension list as the fallback for a package whose app is not
//! installed (the registry then knows nothing of it).

use std::path::Path;

/// True when LaunchServices or the bundle bit says `dir` is a package. Only
/// folders that have an extension are asked.
#[cfg(target_os = "macos")]
pub fn is_package(dir: &Path) -> bool {
    use objc2::rc::{autoreleasepool, Retained};
    use objc2::runtime::AnyObject;
    use objc2_foundation::{NSNumber, NSString, NSURLIsPackageKey, NSURL};

    if dir.extension().is_none() {
        return false;
    }
    let Some(posix) = dir.to_str() else {
        return false;
    };
    autoreleasepool(|_| {
        let url = NSURL::fileURLWithPath(&NSString::from_str(posix));
        let mut value: Option<Retained<AnyObject>> = None;
        // SAFETY: `NSURLIsPackageKey` yields an `NSNumber`, which is how it
        // is read back; any other answer is ignored by the downcast.
        let asked = unsafe { url.getResourceValue_forKey_error(&mut value, NSURLIsPackageKey) };
        asked.is_ok()
            && value
                .and_then(|object| object.downcast::<NSNumber>().ok())
                .is_some_and(|number| number.boolValue())
    })
}

#[cfg(not(target_os = "macos"))]
pub fn is_package(_: &Path) -> bool {
    false
}
