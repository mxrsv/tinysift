//! What kind of thing a Large Files row is, for the page's lens chips
//! (LV-L5): video, installer or disk image, archive, virtual machine, audio,
//! other. A lens filters across groups and never regroups, so it is decided
//! from the file itself (its extension, or the owner of a group 5 item),
//! never from the group it landed in.

use std::path::Path;

use super::discovery::VIDEO_EXTENSIONS;

/// Disk images and installer packages: the "installer or disk image" chip.
pub const INSTALLER_LENS_EXTENSIONS: &[&str] =
    &["dmg", "pkg", "mpkg", "iso", "img", "sparseimage", "toast"];
pub const ARCHIVE_EXTENSIONS: &[&str] = &[
    "zip", "tar", "gz", "tgz", "bz2", "xz", "zst", "7z", "rar", "xip",
];
/// Loose virtual disks; a VM bundle is named by its owner instead.
pub const VIRTUAL_DISK_EXTENSIONS: &[&str] = &["vmdk", "vdi", "qcow2", "vhd", "vhdx"];
pub const AUDIO_EXTENSIONS: &[&str] = &["wav", "aif", "aiff", "flac", "mp3", "m4a", "aac"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lens {
    Video,
    /// An installer package or a disk image.
    Installer,
    Archive,
    VirtualMachine,
    Audio,
    Other,
}

impl Lens {
    /// The stable identifier the clients key on.
    pub fn id(self) -> &'static str {
        match self {
            Self::Video => "video",
            Self::Installer => "installer",
            Self::Archive => "archive",
            Self::VirtualMachine => "virtual_machine",
            Self::Audio => "audio",
            Self::Other => "other",
        }
    }

    /// The lens a file's extension names; `Other` for anything else.
    pub fn of_path(path: &Path) -> Self {
        let Some(extension) = path
            .extension()
            .and_then(|e| e.to_str())
            .map(str::to_lowercase)
        else {
            return Self::Other;
        };
        let is = |list: &[&str]| list.contains(&extension.as_str());
        if is(VIDEO_EXTENSIONS) {
            Self::Video
        } else if is(INSTALLER_LENS_EXTENSIONS) {
            Self::Installer
        } else if is(ARCHIVE_EXTENSIONS) {
            Self::Archive
        } else if is(VIRTUAL_DISK_EXTENSIONS) {
            Self::VirtualMachine
        } else if is(AUDIO_EXTENSIONS) {
            Self::Audio
        } else {
            Self::Other
        }
    }
}
