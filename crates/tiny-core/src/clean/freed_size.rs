//! The bytes deleting a cleanup item would free, counted per inode.
//!
//! Only regular files count; symlinks and directory entries do not.
//!
//! - Allocated blocks count, never the logical length.
//! - An inode with several hard links counts once, and only when every link
//!   is inside the item. Otherwise the remaining link keeps its data.
//! - An APFS full clone (`EF_SHARES_ALL_BLOCKS`) shares every block with the
//!   other files of its clone ID. Their allocated size counts once when all
//!   `ATTR_CMNEXT_CLONE_REFCNT` of them are inside the item, else not at all.
//! - A partly shared file (`EF_MAY_SHARE_BLOCKS` only) counts its
//!   `ATTR_CMNEXT_PRIVATESIZE`. Blocks it shares with another file in the same
//!   item are then missed, so this part can be low.
//! - A file that shares nothing counts its allocated blocks, not its private
//!   size, which would also leave out blocks a local snapshot holds.
//! - When the volume cannot answer (not APFS, a failed query, not macOS),
//!   allocated blocks count and the size is only an upper bound.
//!
//! No attribute shows whether a clone family wholly inside the item still
//! shares blocks with an edited clone elsewhere; such a size is too high.
//!
//! A link or clone family split between two items counts in neither. Each
//! item keeps it as [`SharedBlocks`], so items removed together count it
//! once when they hold every link or member between them ([`union_bytes`]).

use std::collections::HashMap;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::Path;

/// How a file's blocks are shared, as its volume reports it. Only macOS can
/// answer anything but `Unknown`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
pub(crate) enum Sharing {
    /// The volume could not tell.
    Unknown,
    /// Nothing is shared: deleting frees every allocated block.
    Unshared,
    /// Some blocks are shared; only `private` bytes are certainly freed.
    Partial { private: u64 },
    /// Every block is shared with the other files of clone ID `id`, which
    /// has `members` files in total.
    Clone { id: u64, members: u64 },
}

/// What the size rule needs to know about one regular file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FileFacts {
    pub dev: u64,
    pub ino: u64,
    pub nlink: u64,
    pub allocated: u64,
    pub sharing: Sharing,
}

/// What a walk saw of one clone family.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
struct Family {
    allocated: u64,
    members: u64,
    seen: u64,
}

impl Family {
    fn merge(&mut self, other: &Family) {
        self.allocated = self.allocated.max(other.allocated);
        self.members = self.members.max(other.members);
        self.seen += other.seen;
    }
}

/// An inode as `(device, inode)` or a clone family as `(device, clone ID)`.
type Key = (u64, u64);

/// Blocks an item shares with files outside it, which its own size leaves
/// out: hard-linked inodes with a link elsewhere and clone families with a
/// member elsewhere. Items removed together free them once they hold every
/// link and member between them.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SharedBlocks {
    /// Per inode: the links inside the item and the inode's facts.
    links: Vec<(Key, (u64, FileFacts))>,
    /// Per clone family: what the item holds of it.
    families: Vec<(Key, Family)>,
}

impl SharedBlocks {
    pub fn is_empty(&self) -> bool {
        self.links.is_empty() && self.families.is_empty()
    }

    /// Keeps only what some of `items` together could free: an inode or a
    /// family whose every link or member lies in them. The rest can never be
    /// completed by a selection of these items, so it need not be kept.
    pub fn retain_completable(items: &mut [&mut SharedBlocks]) {
        let mut links: HashMap<Key, (u64, FileFacts)> = HashMap::new();
        let mut families: HashMap<Key, Family> = HashMap::new();
        for item in items.iter() {
            for (key, (seen, file)) in &item.links {
                links.entry(*key).or_insert((0, *file)).0 += seen;
            }
            for (key, family) in &item.families {
                families.entry(*key).or_default().merge(family);
            }
        }
        links.retain(|_, (seen, file)| *seen >= file.nlink);
        // A hard-linked clone joins its family once every link is in.
        for (_, file) in links.values() {
            if let Sharing::Clone { id, members } = file.sharing {
                let member = Family {
                    allocated: file.allocated,
                    members,
                    seen: 1,
                };
                families.entry((file.dev, id)).or_default().merge(&member);
            }
        }
        families.retain(|_, family| family.seen >= family.members);
        for item in items.iter_mut() {
            item.links.retain(|(key, _)| links.contains_key(key));
            item.families.retain(|(key, _)| families.contains_key(key));
        }
    }
}

/// What the shared blocks of several items free beyond the items' own sizes
/// when the items are removed together, and whether that is only an upper
/// bound. The items must not overlap: a file walked twice would count as two
/// of its links.
pub fn union_bytes<'a>(shared: impl IntoIterator<Item = &'a SharedBlocks>) -> (u64, bool) {
    // Only the facts already gathered are counted; nothing is queried.
    let mut freed = FreedBytes::with_query(|_| Sharing::Unknown);
    for blocks in shared {
        freed.absorb(blocks);
    }
    let (bytes, upper_bound, _) = freed.finish_shared();
    (bytes, upper_bound)
}

/// Accumulates the freed bytes of one item.
pub(crate) struct FreedBytes {
    query: fn(&Path) -> Sharing,
    total: u64,
    upper_bound: bool,
    /// Inodes with several links, with the links seen inside the item.
    linked: HashMap<(u64, u64), (u64, FileFacts)>,
    families: HashMap<(u64, u64), Family>,
}

impl Default for FreedBytes {
    fn default() -> Self {
        Self::with_query(sharing_of)
    }
}

impl FreedBytes {
    pub(crate) fn with_query(query: fn(&Path) -> Sharing) -> Self {
        Self {
            query,
            total: 0,
            upper_bound: false,
            linked: HashMap::new(),
            families: HashMap::new(),
        }
    }

    /// Adds a regular file inside the item; `meta` is its `symlink_metadata`.
    pub(crate) fn add_file(&mut self, path: &Path, meta: &fs::Metadata) {
        let allocated = meta.blocks().saturating_mul(512);
        if allocated == 0 {
            // Nothing to free, so the volume need not be asked.
            return;
        }
        self.add(FileFacts {
            dev: meta.dev(),
            ino: meta.ino(),
            nlink: meta.nlink(),
            allocated,
            sharing: (self.query)(path),
        });
    }

    pub(crate) fn add(&mut self, file: FileFacts) {
        if file.nlink > 1 {
            self.linked
                .entry((file.dev, file.ino))
                .or_insert((0, file))
                .0 += 1;
        } else {
            self.count(file);
        }
    }

    fn count(&mut self, file: FileFacts) {
        let freed = match file.sharing {
            Sharing::Unknown => {
                self.upper_bound = true;
                file.allocated
            }
            Sharing::Unshared => file.allocated,
            Sharing::Partial { private } => private.min(file.allocated),
            Sharing::Clone { id, members } => {
                let member = Family {
                    allocated: file.allocated,
                    members,
                    seen: 1,
                };
                self.families
                    .entry((file.dev, id))
                    .or_default()
                    .merge(&member);
                0
            }
        };
        self.total = self.total.saturating_add(freed);
    }

    /// Adds another item's shared blocks, as if its files were walked here.
    fn absorb(&mut self, shared: &SharedBlocks) {
        for (key, (seen, file)) in &shared.links {
            self.linked.entry(*key).or_insert((0, *file)).0 += seen;
        }
        for (key, family) in &shared.families {
            self.families.entry(*key).or_default().merge(family);
        }
    }

    /// The item's freed bytes, and whether that is only an upper bound.
    #[cfg(test)]
    pub(crate) fn finish(self) -> (u64, bool) {
        let (bytes, upper_bound, _) = self.finish_shared();
        (bytes, upper_bound)
    }

    /// `finish`, plus the blocks the item shares with files outside it.
    pub(crate) fn finish_shared(mut self) -> (u64, bool, SharedBlocks) {
        let mut shared = SharedBlocks::default();
        for (key, (seen, file)) in std::mem::take(&mut self.linked) {
            if seen >= file.nlink {
                self.count(file);
            } else {
                shared.links.push((key, (seen, file)));
            }
        }
        let mut total = self.total;
        for (key, family) in self.families {
            if family.seen >= family.members {
                total = total.saturating_add(family.allocated);
            } else {
                shared.families.push((key, family));
            }
        }
        // Stable order, so equal walks give equal values.
        shared.links.sort_unstable_by_key(|(key, _)| *key);
        shared.families.sort_unstable_by_key(|(key, _)| *key);
        (total, self.upper_bound, shared)
    }
}

#[cfg(target_os = "macos")]
fn sharing_of(path: &Path) -> Sharing {
    apfs::sharing(path)
}

#[cfg(not(target_os = "macos"))]
fn sharing_of(_: &Path) -> Sharing {
    Sharing::Unknown
}

/// One `getattrlist` call per file for the APFS clone attributes. They are
/// "extended common" attributes, requested in the `forkattr` group with
/// `FSOPT_ATTR_CMN_EXTENDED`.
#[cfg(target_os = "macos")]
mod apfs {
    use super::Sharing;
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    use std::path::Path;

    // Missing from the libc crate; values from <sys/attr.h> and <sys/stat.h>.
    const ATTR_CMNEXT_CLONE_REFCNT: u32 = 0x0000_1000;
    const EF_MAY_SHARE_BLOCKS: u64 = 0x0000_0001;
    const EF_SHARES_ALL_BLOCKS: u64 = 0x0000_0040;

    const REQUESTED: u32 = libc::ATTR_CMNEXT_PRIVATESIZE
        | libc::ATTR_CMNEXT_CLONEID
        | libc::ATTR_CMNEXT_EXT_FLAGS
        | ATTR_CMNEXT_CLONE_REFCNT;
    /// The reply holds a `u32` length and the returned `attribute_set_t`
    /// (five `u32` groups, `forkattr` last), then only the returned values
    /// in bit order, each 4-byte aligned.
    const RETURNED_FORKATTR: usize = 20;
    const VALUES: usize = 24;

    #[repr(C, align(8))]
    struct Reply([u8; 64]);

    pub(super) fn sharing(path: &Path) -> Sharing {
        let Ok(path) = CString::new(path.as_os_str().as_bytes()) else {
            return Sharing::Unknown;
        };
        let mut request = libc::attrlist {
            bitmapcount: libc::ATTR_BIT_MAP_COUNT,
            reserved: 0,
            commonattr: libc::ATTR_CMN_RETURNED_ATTRS,
            volattr: 0,
            dirattr: 0,
            fileattr: 0,
            forkattr: REQUESTED,
        };
        let mut reply = Reply([0; 64]);
        // SAFETY: `path` is NUL-terminated and both buffers outlive the call,
        // which writes at most `reply.0.len()` bytes. `FSOPT_NOFOLLOW` reads
        // a final symlink itself, never its target.
        let rc = unsafe {
            libc::getattrlist(
                path.as_ptr(),
                (&mut request as *mut libc::attrlist).cast(),
                reply.0.as_mut_ptr().cast(),
                reply.0.len(),
                libc::FSOPT_NOFOLLOW | libc::FSOPT_ATTR_CMN_EXTENDED,
            )
        };
        if rc != 0 {
            return Sharing::Unknown;
        }
        parse(&reply.0)
    }

    fn parse(reply: &[u8]) -> Sharing {
        let Some(returned) = read::<4>(reply, RETURNED_FORKATTR).map(u32::from_ne_bytes) else {
            return Sharing::Unknown;
        };
        let mut at = VALUES;
        let mut next = |bit: u32| -> Option<u64> {
            if returned & bit == 0 {
                return None;
            }
            if bit == ATTR_CMNEXT_CLONE_REFCNT {
                let value = read::<4>(reply, at).map(u32::from_ne_bytes)?;
                at += 4;
                return Some(u64::from(value));
            }
            let value = read::<8>(reply, at).map(u64::from_ne_bytes)?;
            at += 8;
            Some(value)
        };
        // Read in bit order, as packed.
        let private = next(libc::ATTR_CMNEXT_PRIVATESIZE);
        let clone_id = next(libc::ATTR_CMNEXT_CLONEID);
        let flags = next(libc::ATTR_CMNEXT_EXT_FLAGS);
        let members = next(ATTR_CMNEXT_CLONE_REFCNT);
        classify(private, clone_id, flags, members)
    }

    fn read<const N: usize>(reply: &[u8], at: usize) -> Option<[u8; N]> {
        reply.get(at..at + N)?.try_into().ok()
    }

    fn classify(
        private: Option<u64>,
        clone_id: Option<u64>,
        flags: Option<u64>,
        members: Option<u64>,
    ) -> Sharing {
        let (Some(private), Some(flags)) = (private, flags) else {
            return Sharing::Unknown;
        };
        // `off_t`: a negative value would be nonsense, so it frees nothing.
        let private = if (private as i64) < 0 { 0 } else { private };
        if flags & EF_SHARES_ALL_BLOCKS != 0 {
            if let (Some(id), Some(members @ 2..)) = (clone_id, members) {
                return Sharing::Clone { id, members };
            }
            // Clone of something the family count does not cover.
            return Sharing::Partial { private };
        }
        if flags & EF_MAY_SHARE_BLOCKS != 0 {
            return Sharing::Partial { private };
        }
        Sharing::Unshared
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        fn reply(returned: u32, values: &[&[u8]]) -> Vec<u8> {
            let mut out = vec![0u8; VALUES];
            out[RETURNED_FORKATTR..VALUES].copy_from_slice(&returned.to_ne_bytes());
            for value in values {
                out.extend_from_slice(value);
            }
            out
        }

        #[test]
        fn parses_only_the_returned_attributes_in_bit_order() {
            let all = reply(
                REQUESTED,
                &[
                    &0u64.to_ne_bytes(),
                    &9u64.to_ne_bytes(),
                    &(EF_MAY_SHARE_BLOCKS | EF_SHARES_ALL_BLOCKS).to_ne_bytes(),
                    &3u32.to_ne_bytes(),
                ],
            );
            assert_eq!(parse(&all), Sharing::Clone { id: 9, members: 3 });

            let no_refcnt = reply(
                REQUESTED & !ATTR_CMNEXT_CLONE_REFCNT,
                &[
                    &0u64.to_ne_bytes(),
                    &9u64.to_ne_bytes(),
                    &EF_SHARES_ALL_BLOCKS.to_ne_bytes(),
                ],
            );
            assert_eq!(parse(&no_refcnt), Sharing::Partial { private: 0 });

            let edited = reply(
                REQUESTED,
                &[
                    &16_384u64.to_ne_bytes(),
                    &4u64.to_ne_bytes(),
                    &EF_MAY_SHARE_BLOCKS.to_ne_bytes(),
                    &1u32.to_ne_bytes(),
                ],
            );
            assert_eq!(parse(&edited), Sharing::Partial { private: 16_384 });

            let unshared = reply(
                REQUESTED,
                &[
                    &4096u64.to_ne_bytes(),
                    &5u64.to_ne_bytes(),
                    &0u64.to_ne_bytes(),
                    &1u32.to_ne_bytes(),
                ],
            );
            assert_eq!(parse(&unshared), Sharing::Unshared);
        }

        #[test]
        fn a_volume_without_the_attributes_is_unknown() {
            assert_eq!(parse(&reply(0, &[])), Sharing::Unknown);
            let flags_only = reply(libc::ATTR_CMNEXT_EXT_FLAGS, &[&0u64.to_ne_bytes()]);
            assert_eq!(parse(&flags_only), Sharing::Unknown);
            assert_eq!(parse(&[0u8; 8]), Sharing::Unknown, "short reply");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const BLOCK: u64 = 4096;

    fn file(ino: u64, nlink: u64, sharing: Sharing) -> FileFacts {
        FileFacts {
            dev: 1,
            ino,
            nlink,
            allocated: 3 * BLOCK,
            sharing,
        }
    }

    fn measure(files: &[FileFacts]) -> (u64, bool) {
        let mut freed = FreedBytes::default();
        for file in files {
            freed.add(*file);
        }
        freed.finish()
    }

    #[test]
    fn unshared_files_count_their_allocated_blocks() {
        let files = [file(1, 1, Sharing::Unshared), file(2, 1, Sharing::Unshared)];
        assert_eq!(measure(&files), (6 * BLOCK, false));
    }

    #[test]
    fn a_hard_link_counts_once_and_only_when_every_link_is_inside() {
        let linked = file(1, 2, Sharing::Unshared);
        assert_eq!(measure(&[linked, linked]), (3 * BLOCK, false));
        assert_eq!(measure(&[linked]), (0, false), "a link outside the item");
    }

    #[test]
    fn a_clone_family_counts_once_and_only_when_every_member_is_inside() {
        let member = |ino| file(ino, 1, Sharing::Clone { id: 7, members: 2 });
        assert_eq!(measure(&[member(1), member(2)]), (3 * BLOCK, false));
        assert_eq!(measure(&[member(1)]), (0, false), "a member outside");
        let other_volume = FileFacts {
            dev: 2,
            ..member(2)
        };
        assert_eq!(measure(&[member(1), other_volume]), (0, false));
    }

    #[test]
    fn a_clone_member_counts_only_with_every_link_inside() {
        let linked = file(1, 2, Sharing::Clone { id: 7, members: 2 });
        let other = file(2, 1, Sharing::Clone { id: 7, members: 2 });
        assert_eq!(measure(&[linked, other]), (0, false));
        assert_eq!(measure(&[linked, other, linked]), (3 * BLOCK, false));
    }

    #[test]
    fn a_partly_shared_file_counts_its_private_bytes() {
        let edited = file(1, 1, Sharing::Partial { private: BLOCK });
        assert_eq!(measure(&[edited]), (BLOCK, false));
        let odd = file(
            2,
            1,
            Sharing::Partial {
                private: 99 * BLOCK,
            },
        );
        assert_eq!(measure(&[odd]), (3 * BLOCK, false), "never above allocated");
    }

    /// What an item made of `files` shares with files outside it.
    fn shared(files: &[FileFacts]) -> SharedBlocks {
        let mut freed = FreedBytes::default();
        for file in files {
            freed.add(*file);
        }
        freed.finish_shared().2
    }

    #[test]
    fn a_link_or_family_split_between_items_counts_once_in_their_union() {
        let linked = file(1, 2, Sharing::Unshared);
        let (a, b) = (shared(&[linked]), shared(&[linked]));
        assert_eq!(union_bytes([&a]), (0, false), "one item alone");
        assert_eq!(union_bytes([&a, &b]), (3 * BLOCK, false), "once");

        let member = |ino| file(ino, 1, Sharing::Clone { id: 7, members: 3 });
        let (a, b) = (shared(&[member(1), member(2)]), shared(&[member(3)]));
        assert_eq!(union_bytes([&a]), (0, false));
        assert_eq!(union_bytes([&a, &b]), (3 * BLOCK, false));

        // A clone with one link in each item joins its family only in the union.
        let linked_clone = file(4, 2, Sharing::Clone { id: 8, members: 2 });
        let other = file(5, 1, Sharing::Clone { id: 8, members: 2 });
        let (a, b) = (shared(&[linked_clone, other]), shared(&[linked_clone]));
        assert_eq!(union_bytes([&a, &b]), (3 * BLOCK, false));

        let unknown = file(6, 2, Sharing::Unknown);
        let (a, b) = (shared(&[unknown]), shared(&[unknown]));
        assert_eq!(union_bytes([&a, &b]), (3 * BLOCK, true), "upper bound");
    }

    #[test]
    fn only_what_the_items_could_complete_together_is_kept() {
        let linked = file(1, 2, Sharing::Unshared);
        let with_outside = file(2, 3, Sharing::Unshared);
        let member = |ino| file(ino, 1, Sharing::Clone { id: 7, members: 3 });
        let mut a = shared(&[linked, with_outside, member(3)]);
        let mut b = shared(&[linked, with_outside, member(4)]);
        SharedBlocks::retain_completable(&mut [&mut a, &mut b]);
        assert_eq!(a, shared(&[linked]), "a third link or member is elsewhere");
        assert_eq!(b, shared(&[linked]));
        assert_eq!(union_bytes([&a, &b]), (3 * BLOCK, false));
    }

    #[test]
    fn an_unknown_answer_counts_allocated_blocks_as_an_upper_bound() {
        let files = [file(1, 1, Sharing::Unknown), file(2, 1, Sharing::Unshared)];
        assert_eq!(measure(&files), (6 * BLOCK, true));
    }
}
