//! A bounded look at an MKV's structure before the `matroska` crate sees it.
//!
//! The crate allocates `vec![0; declared_size]` for every element before it
//! checks that the size fits the parent or the file, and an allocation that
//! fails aborts the process (`catch_unwind` cannot stop that). It also recurses
//! once per nested master element and follows a SeekHead chain without a
//! limit. [`is_safe`] walks exactly the parts the crate reads (the EBML header
//! elements, the Segment's children up to the first SeekHead, the elements a
//! SeekHead points at) and refuses a file where any declared size is larger
//! than what is left of its parent, where masters nest too deep or where
//! SeekHeads loop. It reads headers only and allocates nothing from the file.

use std::collections::HashSet;
use std::io::{Read, Seek, SeekFrom};

const SEGMENT: u32 = 0x1853_8067;
const SEEKHEAD: u32 = 0x114D_9B74;
const INFO: u32 = 0x1549_A966;
const TRACKS: u32 = 0x1654_AE6B;
const ATTACHMENTS: u32 = 0x1941_A469;
const CHAPTERS: u32 = 0x1043_A770;
const TAGS: u32 = 0x1254_C367;
const SEEK: u32 = 0x4DBB;
const SEEK_ID: u32 = 0x53AB;
const SEEK_POSITION: u32 = 0x53AC;
/// The parts the crate parses into memory.
const PARTS: [u32; 5] = [INFO, TRACKS, ATTACHMENTS, CHAPTERS, TAGS];
/// Real files nest four or five levels; the crate recurses without a limit.
const MAX_DEPTH: u32 = 16;
/// Real files have one or two SeekHeads.
const MAX_SEEKHEADS: usize = 8;
/// Element IDs the crate reads as masters wherever the parent is not one of
/// the five parts (`ebml.rs` `IDS_MASTER_DEFAULT`).
const NESTED_MASTERS: &[u32] = &[
    0x80,
    0x8E,
    0x8F,
    0xA0,
    0xA6,
    0xAE,
    0xB6,
    0xB7,
    0xBB,
    0xC8,
    0xDB,
    0xE0,
    0xE1,
    0xE2,
    0xE3,
    0xE4,
    0xE8,
    0xE9,
    0x45B9,
    0x4DBB,
    0x5034,
    0x5035,
    0x55B0,
    0x55D0,
    0x5854,
    0x61A7,
    0x6240,
    0x63C0,
    0x6624,
    0x67C8,
    0x6911,
    0x6924,
    0x6944,
    0x6D80,
    0x7373,
    0x75A1,
    0x7E5B,
    0x7E7B,
    0x1043_A770,
    0x114D_9B74,
    0x1254_C367,
    0x1549_A966,
    0x1654_AE6B,
    0x1853_8067,
    0x1941_A469,
    0x1A45_DFA3,
    0x1B53_8667,
    0x1C53_BB6B,
    0x1F43_B675,
];

/// True when handing `file` (of `len` bytes) to the crate cannot allocate more
/// than the file holds, recurse deeply or loop.
pub fn is_safe<R: Read + Seek>(file: &mut R, len: u64) -> bool {
    check(&mut Walk { file, len }).is_some()
}

struct Walk<'a, R> {
    file: &'a mut R,
    len: u64,
}

/// An element header: ID, declared body size and where the body starts.
struct Head {
    id: u32,
    size: u64,
    body: u64,
}

impl<R: Read + Seek> Walk<'_, R> {
    fn head(&mut self, at: u64) -> Option<Head> {
        self.file.seek(SeekFrom::Start(at)).ok()?;
        let mut first = [0u8; 1];
        self.file.read_exact(&mut first).ok()?;
        let id_len = first[0].leading_zeros() as usize + 1;
        if id_len > 4 {
            return None;
        }
        let mut id = u32::from(first[0]);
        for _ in 1..id_len {
            id = (id << 8) | u32::from(self.byte()?);
        }
        let first = self.byte()?;
        let size_len = first.leading_zeros() as usize + 1;
        if size_len > 8 {
            return None;
        }
        let mut size = u64::from(first) & (0xFF >> size_len);
        for _ in 1..size_len {
            size = (size << 8) | u64::from(self.byte()?);
        }
        Some(Head {
            id,
            size,
            body: at + id_len as u64 + size_len as u64,
        })
    }

    fn byte(&mut self) -> Option<u8> {
        let mut byte = [0u8; 1];
        self.file.read_exact(&mut byte).ok()?;
        Some(byte[0])
    }

    /// The end of `head`'s body, when the file holds all of it.
    fn end_within_file(&self, head: &Head) -> Option<u64> {
        let end = head.body.checked_add(head.size)?;
        (end <= self.len).then_some(end)
    }

    /// `head` is a part or SeekHead the crate reads: every element inside it
    /// must fit its parent.
    fn check_master(&mut self, head: &Head, parent: u32, depth: u32) -> Option<()> {
        if depth > MAX_DEPTH {
            return None;
        }
        let end = self.end_within_file(head)?;
        let mut at = head.body;
        while at < end {
            let child = self.head(at)?;
            let child_end = child.body.checked_add(child.size)?;
            if child_end > end {
                return None;
            }
            if is_master(parent, child.id) {
                self.check_master(&child, child.id, depth + 1)?;
            }
            at = child_end;
        }
        Some(())
    }

    /// A SeekHead's `(id, position)` entries, read after it passed
    /// `check_master`, so every size inside is within bounds.
    fn seek_entries(&mut self, head: &Head) -> Option<Vec<(u32, u64)>> {
        let end = self.end_within_file(head)?;
        let mut entries = Vec::new();
        let mut at = head.body;
        while at < end {
            let seek = self.head(at)?;
            let seek_end = seek.body.checked_add(seek.size)?;
            if seek.id == SEEK {
                let (mut id, mut position) = (None, None);
                let mut inner = seek.body;
                while inner < seek_end {
                    let field = self.head(inner)?;
                    let field_end = field.body.checked_add(field.size)?;
                    if field.size > 8 {
                        return None;
                    }
                    let value = self.uint(&field)?;
                    match field.id {
                        SEEK_ID => id = u32::try_from(value).ok(),
                        SEEK_POSITION => position = Some(value),
                        _ => {}
                    }
                    inner = field_end;
                }
                if let (Some(id), Some(position)) = (id, position) {
                    entries.push((id, position));
                }
            }
            at = seek_end;
        }
        Some(entries)
    }

    fn uint(&mut self, field: &Head) -> Option<u64> {
        self.file.seek(SeekFrom::Start(field.body)).ok()?;
        let mut value = 0u64;
        for _ in 0..field.size {
            value = (value << 8) | u64::from(self.byte()?);
        }
        Some(value)
    }
}

/// How the crate decides a child is a master (`ebml.rs`, `IDS_MASTER`).
fn is_master(parent: u32, child: u32) -> bool {
    match parent {
        INFO => child == 0x6924,
        TRACKS => matches!(child, 0x6924 | 0xAE),
        ATTACHMENTS => child == 0x61A7,
        CHAPTERS => child == 0x45B9,
        TAGS => child == 0x7373,
        _ => NESTED_MASTERS.contains(&child),
    }
}

fn check<R: Read + Seek>(walk: &mut Walk<'_, R>) -> Option<()> {
    // Elements before the Segment are skipped by their declared size.
    let mut at = 0;
    let segment = loop {
        let head = walk.head(at)?;
        if head.id == SEGMENT {
            break head;
        }
        at = walk.end_within_file(&head)?;
    };
    // The crate walks the Segment's declared size, not the file's end.
    let segment_end = segment.body.saturating_add(segment.size).min(walk.len);
    let mut at = segment.body;
    while at < segment_end {
        let head = walk.head(at)?;
        match head.id {
            SEEKHEAD => return check_seek_heads(walk, &head, segment.body),
            id if PARTS.contains(&id) => walk.check_master(&head, id, 0)?,
            _ => {}
        }
        // Skipped by seeking; a size past the end only ends the walk, where the
        // crate then fails on its own read.
        at = head.body.saturating_add(head.size);
    }
    Some(())
}

/// The crate stops walking at the first SeekHead and jumps to what it lists,
/// following a chain of SeekHeads: each target gets the same bounds.
fn check_seek_heads<R: Read + Seek>(
    walk: &mut Walk<'_, R>,
    first: &Head,
    segment_start: u64,
) -> Option<()> {
    let mut seen = HashSet::new();
    let mut pending = vec![(first.body, true)];
    let mut heads = 0;
    while let Some((body, _)) = pending.pop() {
        heads += 1;
        if heads > MAX_SEEKHEADS || !seen.insert(body) {
            return None;
        }
        let head = if body == first.body {
            Head {
                id: SEEKHEAD,
                size: first.size,
                body,
            }
        } else {
            let head = walk.head(body)?;
            if head.id != SEEKHEAD {
                return None;
            }
            head
        };
        walk.check_master(&head, SEGMENT, 0)?;
        for (id, position) in walk.seek_entries(&head)? {
            let target = segment_start.checked_add(position)?;
            if id == SEEKHEAD {
                let found = walk.head(target)?;
                pending.push((found.body, true));
            } else if PARTS.contains(&id) {
                let part = walk.head(target)?;
                // A wrong ID makes the crate panic before it allocates.
                if part.id == id {
                    walk.check_master(&part, id, 0)?;
                }
            }
        }
    }
    Some(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn element(id: &[u8], body: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        // An 8-byte size keeps every fixture simple.
        out.push(0x01);
        out.extend_from_slice(&(body.len() as u64).to_be_bytes()[1..]);
        out.extend_from_slice(body);
        out
    }

    fn declared(id: &[u8], size: u64, body: &[u8]) -> Vec<u8> {
        let mut out = id.to_vec();
        out.push(0x01);
        out.extend_from_slice(&size.to_be_bytes()[1..]);
        out.extend_from_slice(body);
        out
    }

    fn mkv(segment_children: &[u8]) -> Vec<u8> {
        let mut file = element(
            &[0x1A, 0x45, 0xDF, 0xA3],
            &element(&[0x42, 0x82], b"matroska"),
        );
        file.extend(element(&[0x18, 0x53, 0x80, 0x67], segment_children));
        file
    }

    fn safe(bytes: &[u8]) -> bool {
        is_safe(&mut Cursor::new(bytes), bytes.len() as u64)
    }

    #[test]
    fn a_well_formed_file_passes() {
        let info = element(&[0x15, 0x49, 0xA9, 0x66], &element(&[0x44, 0x89], &[0; 8]));
        let track = element(&[0xAE], &element(&[0xD7], &[1]));
        let tracks = element(&[0x16, 0x54, 0xAE, 0x6B], &track);
        assert!(safe(&mkv(&[info, tracks].concat())));
    }

    #[test]
    fn an_oversized_info_is_refused() {
        let info = declared(&[0x15, 0x49, 0xA9, 0x66], 1 << 48, &[0; 8]);
        assert!(!safe(&mkv(&info)));
    }

    #[test]
    fn an_oversized_element_inside_a_part_is_refused() {
        // The part is small and honest; the one inside it claims a terabyte.
        let inner = declared(&[0x73, 0xA4], 1 << 40, &[0; 4]);
        let tags = element(&[0x12, 0x54, 0xC3, 0x67], &inner);
        assert!(!safe(&mkv(&tags)));
    }

    #[test]
    fn a_truncated_file_is_refused() {
        let info = element(&[0x15, 0x49, 0xA9, 0x66], &element(&[0x44, 0x89], &[0; 8]));
        let whole = mkv(&info);
        assert!(!safe(&whole[..whole.len() - 5]));
        assert!(!safe(&whole[..10]));
    }

    #[test]
    fn masters_nested_without_end_are_refused() {
        let mut body = element(&[0xD7], &[1]);
        for _ in 0..40 {
            body = element(&[0xE0], &body);
        }
        let tracks = element(&[0x16, 0x54, 0xAE, 0x6B], &element(&[0xAE], &body));
        assert!(!safe(&mkv(&tracks)));
    }

    #[test]
    fn a_seek_head_that_points_at_an_oversized_part_or_at_itself_is_refused() {
        let entry = |id: &[u8], position: u64| {
            let mut body = element(&[0x53, 0xAB], id);
            body.extend(element(&[0x53, 0xAC], &position.to_be_bytes()));
            element(&[0x4D, 0xBB], &body)
        };
        // The Segment body starts right after its header: the SeekHead is
        // first, the bad Info follows at the position the entry names.
        let seek_head_len = element(
            &[0x11, 0x4D, 0x9B, 0x74],
            &entry(&[0x15, 0x49, 0xA9, 0x66], 0),
        )
        .len();
        let info = declared(&[0x15, 0x49, 0xA9, 0x66], 1 << 48, &[0; 8]);
        let head = element(
            &[0x11, 0x4D, 0x9B, 0x74],
            &entry(&[0x15, 0x49, 0xA9, 0x66], seek_head_len as u64),
        );
        assert!(!safe(&mkv(&[head, info].concat())));
        // A SeekHead listing itself would make the crate loop forever.
        let looped = element(
            &[0x11, 0x4D, 0x9B, 0x74],
            &entry(&[0x11, 0x4D, 0x9B, 0x74], 0),
        );
        assert!(!safe(&mkv(&looped)));
    }
}
