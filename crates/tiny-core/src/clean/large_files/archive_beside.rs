//! Group 1, an archive whose contents sit extracted beside it (LV-L6): a
//! `.zip` is a copy when every file it lists is in the folder beside it with
//! the same size. The fact is checked, never guessed from names: the
//! archive's central directory is read (names, sizes, kinds; nothing is
//! decompressed) and each entry is looked up on disk.
//!
//! Where the contents sit follows Archive Utility: an archive whose entries
//! share one top-level item (`Foo/…`, or a single file) unpacks to that item
//! beside it; any other unpacks into a folder named after the archive. Only
//! `.zip` is read: a compressed tarball would have to be decompressed whole
//! to list it, so it stays an archive of group 3.

use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};

use crate::clean::fs_safe::PathFingerprint;

use super::discovery::is_dataless;

/// An archive with more entries is not checked (one `lstat` per entry).
const MAX_ENTRIES: u64 = 50_000;
/// A larger central directory is not read.
const MAX_DIRECTORY_BYTES: u64 = 32 * 1024 * 1024;
const EOCD_SIGNATURE: u32 = 0x0605_4b50;
const EOCD_MIN: usize = 22;
const ZIP64_LOCATOR_SIGNATURE: u32 = 0x0706_4b50;
const ZIP64_EOCD_SIGNATURE: u32 = 0x0606_4b50;
const ENTRY_SIGNATURE: u32 = 0x0201_4b50;
const ENTRY_HEADER: usize = 46;
const ZIP64_EXTRA_ID: u16 = 0x0001;
/// Unix file type bits in an entry's external attributes, `S_IFLNK`.
const MODE_TYPE_MASK: u32 = 0o170_000;
const MODE_SYMLINK: u32 = 0o120_000;
/// "Made by" Unix, the host whose external attributes carry a mode.
const HOST_UNIX: u16 = 3;

/// The checked relation behind an archive's Copies row.
#[derive(Debug, Clone)]
pub struct Extracted {
    /// The unpacked item beside the archive: a folder, or the one file.
    pub beside: PathBuf,
    /// Files, folders and links the archive lists (resource forks left out).
    pub entries: u64,
    /// The largest unpacked file and how the scan saw it: a move of the
    /// archive proceeds only while it is still there, unchanged.
    pub anchor: PathBuf,
    pub anchor_fingerprint: PathFingerprint,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Entry {
    pub name: String,
    pub size: u64,
    pub kind: EntryKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EntryKind {
    File,
    Folder,
    Symlink,
}

/// True for the archives this module can read.
pub fn is_checked_archive(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("zip"))
}

/// The unpacked copy beside `archive`, when every entry is there as listed.
pub fn extracted_beside(archive: &Path) -> Option<Extracted> {
    if !is_checked_archive(archive) {
        return None;
    }
    let entries = read_entries(archive)?;
    let parent = archive.parent()?;
    let tops: Vec<&str> = entries
        .iter()
        .filter_map(|e| e.name.split('/').next())
        .collect();
    let single_top = tops
        .first()
        .filter(|first| tops.iter().all(|t| t == *first));
    let (base, beside) = match single_top {
        Some(top) => (parent.to_path_buf(), parent.join(top)),
        None => {
            let folder = parent.join(archive.file_stem()?);
            (folder.clone(), folder)
        }
    };
    if beside == archive {
        return None;
    }
    let mut anchor: Option<(u64, PathBuf, fs::Metadata)> = None;
    for entry in &entries {
        let path = base.join(entry.name.trim_end_matches('/'));
        let meta = fs::symlink_metadata(&path).ok()?;
        match entry.kind {
            EntryKind::Folder if !meta.is_dir() => return None,
            EntryKind::Symlink if !meta.file_type().is_symlink() => return None,
            EntryKind::File => {
                // Bytes in iCloud only are not a copy on this Mac.
                if !meta.is_file() || meta.len() != entry.size || is_dataless(&meta) {
                    return None;
                }
                if anchor.as_ref().is_none_or(|(size, ..)| entry.size > *size) {
                    anchor = Some((entry.size, path, meta));
                }
            }
            _ => {}
        }
    }
    let (_, anchor, meta) = anchor?;
    Some(Extracted {
        beside,
        entries: entries.len() as u64,
        anchor_fingerprint: PathFingerprint::of(&meta),
        anchor,
    })
}

/// The archive's entries, `None` when it is not a readable zip, is too large
/// to check, or names a path that could leave its folder.
pub(crate) fn read_entries(archive: &Path) -> Option<Vec<Entry>> {
    let mut file = File::open(archive).ok()?;
    let length = file.metadata().ok()?.len();
    let (count, size, offset) = directory_bounds(&mut file, length)?;
    if count > MAX_ENTRIES || size > MAX_DIRECTORY_BYTES || offset.checked_add(size)? > length {
        return None;
    }
    let mut directory = vec![0u8; usize::try_from(size).ok()?];
    file.seek(SeekFrom::Start(offset)).ok()?;
    file.read_exact(&mut directory).ok()?;
    parse_directory(&directory, count)
}

/// Entry count, size and offset of the central directory, from the end
/// record (or its Zip64 form when a field overflowed).
fn directory_bounds(file: &mut File, length: u64) -> Option<(u64, u64, u64)> {
    let tail_len = length.min((EOCD_MIN + usize::from(u16::MAX)) as u64);
    let tail_start = length - tail_len;
    let mut tail = vec![0u8; usize::try_from(tail_len).ok()?];
    file.seek(SeekFrom::Start(tail_start)).ok()?;
    file.read_exact(&mut tail).ok()?;
    let at = (0..=tail.len().checked_sub(EOCD_MIN)?)
        .rev()
        .find(|&i| le32(&tail, i) == Some(EOCD_SIGNATURE))?;
    let count = u64::from(le16(&tail, at + 10)?);
    let size = u64::from(le32(&tail, at + 12)?);
    let offset = u64::from(le32(&tail, at + 16)?);
    if count != 0xFFFF && size != 0xFFFF_FFFF && offset != 0xFFFF_FFFF {
        return Some((count, size, offset));
    }
    // Zip64: the locator sits right before the end record.
    let locator = at.checked_sub(20)?;
    if le32(&tail, locator)? != ZIP64_LOCATOR_SIGNATURE {
        return None;
    }
    let record_at = le64(&tail, locator + 8)?;
    let mut record = [0u8; 56];
    file.seek(SeekFrom::Start(record_at)).ok()?;
    file.read_exact(&mut record).ok()?;
    if le32(&record, 0)? != ZIP64_EOCD_SIGNATURE {
        return None;
    }
    Some((le64(&record, 32)?, le64(&record, 40)?, le64(&record, 48)?))
}

/// The central directory's entries, in order; `__MACOSX/` resource forks and
/// Finder's `.DS_Store` are left out.
pub(crate) fn parse_directory(data: &[u8], count: u64) -> Option<Vec<Entry>> {
    let mut entries = Vec::new();
    let mut at = 0usize;
    for _ in 0..count {
        if le32(data, at)? != ENTRY_SIGNATURE {
            return None;
        }
        let made_by_host = le16(data, at + 4)? >> 8;
        let mut size = u64::from(le32(data, at + 24)?);
        let name_len = usize::from(le16(data, at + 28)?);
        let extra_len = usize::from(le16(data, at + 30)?);
        let comment_len = usize::from(le16(data, at + 32)?);
        let external = le32(data, at + 38)?;
        let name_at = at + ENTRY_HEADER;
        let name = data.get(name_at..name_at.checked_add(name_len)?)?;
        let name = String::from_utf8(name.to_vec()).ok()?;
        if size == 0xFFFF_FFFF {
            let extra = data.get(name_at + name_len..name_at + name_len + extra_len)?;
            size = zip64_size(extra)?;
        }
        at = name_at + name_len + extra_len + comment_len;
        if !is_safe_name(&name) {
            return None;
        }
        if name.starts_with("__MACOSX/")
            || name.rsplit('/').next() == Some(".DS_Store")
            || name.trim_end_matches('/').is_empty()
        {
            continue;
        }
        let kind = if name.ends_with('/') {
            EntryKind::Folder
        } else if made_by_host == HOST_UNIX && (external >> 16) & MODE_TYPE_MASK == MODE_SYMLINK {
            EntryKind::Symlink
        } else {
            EntryKind::File
        };
        entries.push(Entry { name, size, kind });
    }
    (!entries.is_empty()).then_some(entries)
}

/// The uncompressed size in a Zip64 extra field: its first value.
fn zip64_size(extra: &[u8]) -> Option<u64> {
    let mut at = 0;
    while at + 4 <= extra.len() {
        let id = le16(extra, at)?;
        let len = usize::from(le16(extra, at + 2)?);
        if id == ZIP64_EXTRA_ID {
            return le64(extra, at + 4);
        }
        at += 4 + len;
    }
    None
}

/// A relative path that stays inside the folder it unpacks to.
fn is_safe_name(name: &str) -> bool {
    !name.is_empty()
        && !name.contains('\\')
        && Path::new(name)
            .components()
            .all(|c| matches!(c, Component::Normal(_)))
}

fn le16(data: &[u8], at: usize) -> Option<u16> {
    Some(u16::from_le_bytes(data.get(at..at + 2)?.try_into().ok()?))
}

fn le32(data: &[u8], at: usize) -> Option<u32> {
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

fn le64(data: &[u8], at: usize) -> Option<u64> {
    Some(u64::from_le_bytes(data.get(at..at + 8)?.try_into().ok()?))
}
