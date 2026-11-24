//! The reasons of groups 3 and 4 (LR1): each names the fact the scan checked,
//! the kind from the extension, the site from the where-from attribute, and
//! the date. Kept apart from the walk so the walk stays about what it visits.

use std::path::Path;
use std::time::{Duration, SystemTime};

use super::last_used::LastUsed;
use super::lens::{ARCHIVE_EXTENSIONS, AUDIO_EXTENSIONS};
use super::VIDEO_EXTENSIONS;

const SECONDS_PER_DAY: u64 = 86_400;

/// Disk images that are not an installer package: group 3 when no installed
/// app matched them (the installer scan claims those first).
const DISK_IMAGE_EXTENSIONS: &[&str] = &["dmg", "iso", "img", "sparseimage", "toast"];
const DUMP_EXTENSIONS: &[&str] = &["sql", "dump"];

/// The word group 3 names a file by, when its kind is one group 3 holds
/// (LV-L6): a video, audio, a disk image, an archive, a database dump.
pub(super) fn only_copy_kind(extension: Option<&str>) -> Option<&'static str> {
    let extension = extension?;
    let is = |list: &[&str]| list.contains(&extension);
    if is(VIDEO_EXTENSIONS) {
        Some("Video")
    } else if is(AUDIO_EXTENSIONS) {
        Some("Audio")
    } else if is(DISK_IMAGE_EXTENSIONS) {
        Some("Disk image")
    } else if is(ARCHIVE_EXTENSIONS) {
        Some("Archive")
    } else if is(DUMP_EXTENSIONS) {
        Some("Database dump")
    } else {
        None
    }
}

/// "Video · changed 3 days ago", or "Disk image from github.com · …".
pub(super) fn only_copy_reason(kind: &str, from: Option<&str>, days: u64) -> String {
    format!("{} · changed {}", with_site(kind, from), ago(days))
}

/// Group 4's age in days and the verb that names its fact, `None` while the
/// file is too recent. Spotlight's date wins only when it is newer, and
/// then the sentence says "used", because that is the fact it checked.
pub(super) fn idle_days(
    path: &Path,
    modified: SystemTime,
    now: SystemTime,
    min_days: u64,
    last_used: &dyn LastUsed,
) -> Option<(u64, &'static str, SystemTime)> {
    // Spotlight is asked only once the modification date alone passes.
    if days_between(modified, now) < min_days {
        return None;
    }
    let used = last_used.last_used(path).filter(|used| *used > modified);
    let (at, verb) = match used {
        Some(used) => (used, "used"),
        None => (modified, "changed"),
    };
    let days = days_between(at, now);
    (days >= min_days).then_some((days, verb, at))
}

/// "Archive · not changed for 120 days", with the site when it is known.
pub(super) fn other_reason(
    extension: Option<&str>,
    from: Option<&str>,
    days: u64,
    verb: &str,
) -> String {
    format!(
        "{} · not {verb} for {days} days",
        with_site(&kind_of(extension), from)
    )
}

fn with_site(kind: &str, from: Option<&str>) -> String {
    match from {
        Some(site) => format!("{kind} from {site}"),
        None => kind.to_string(),
    }
}

pub(super) fn days_between(then: SystemTime, now: SystemTime) -> u64 {
    now.duration_since(then).unwrap_or(Duration::ZERO).as_secs() / SECONDS_PER_DAY
}

fn ago(days: u64) -> String {
    match days {
        0 => "today".into(),
        1 => "1 day ago".into(),
        n => format!("{n} days ago"),
    }
}

/// What the extension says the file is; the extension itself otherwise.
fn kind_of(extension: Option<&str>) -> String {
    let kind = match extension {
        Some("dmg" | "iso" | "img" | "sparseimage") => "Disk image",
        Some("pkg" | "mpkg" | "xip") => "Installer package",
        Some("zip" | "tar" | "gz" | "tgz" | "bz2" | "xz" | "zst" | "7z" | "rar") => "Archive",
        Some("vmdk" | "vdi" | "qcow2" | "vhd" | "vhdx") => "Virtual disk",
        Some("wav" | "aif" | "aiff" | "flac" | "mp3" | "m4a" | "aac") => "Audio",
        Some("psd" | "tif" | "tiff" | "raw" | "dng") => "Image",
        Some("pdf") => "PDF",
        Some(other) if !other.is_empty() => return format!("{} file", other.to_uppercase()),
        _ => "File",
    };
    kind.to_string()
}
