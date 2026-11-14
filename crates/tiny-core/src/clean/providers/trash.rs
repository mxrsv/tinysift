use crate::engine_error;
use crate::error::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::CleanProvider;
use crate::clean::freed_size::FreedBytes;
use crate::clean::fs_safe::{finish_freed, walk_files};
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, Evidence, ExecAction, ExecReport, RiskLevel};

const ID: &str = "trash";
const LABEL: &str = "User Trash";
/// How many of the Trash's largest top-level items the candidate names; the
/// rest are one `TrashRest`.
const LISTED: usize = 5;
/// Finder's folder settings, never something the user put in the Trash.
const FINDER_FILES: [&str; 2] = [".DS_Store", ".localized"];

pub struct TrashProvider;

impl CleanProvider for TrashProvider {
    fn id(&self) -> &'static str {
        ID
    }
    fn label(&self) -> &'static str {
        LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Items already in the Trash; emptying it is permanent".into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Destructive
    }
    fn desktop_empties_trash(&self) -> bool {
        true
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let h = match std::env::var_os("HOME").map(PathBuf::from) {
            Some(h) => h,
            None => return Ok(Vec::new()),
        };
        let root = h.join(".Trash");
        if !root.exists() {
            return Ok(Vec::new());
        }
        // The desktop empties only when discovery listed this Trash, but Finder
        // empties every volume's Trash and whatever arrived since the listing.
        ctx.add_root(&root);
        Ok(vec![trash_item(root, ctx)])
    }

    /// Trash provider rejects every action except `EmptyTrash`. The
    /// orchestrator (`execute.rs`) is responsible for producing this
    /// mapping; tests here pin the rejection behaviour.
    fn execute(&self, _items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        match action {
            ExecAction::EmptyTrash => empty_trash(),
            ExecAction::Trash => Err(engine_error!(
                "trash provider rejects ExecAction::Trash — empty Trash via Hard delete only"
            )),
            ExecAction::HardDelete => Err(engine_error!(
                "trash provider rejects ExecAction::HardDelete — execute.rs must map UI HardDelete → ExecAction::EmptyTrash for trash"
            )),
        }
    }
}

/// The one Trash candidate: its freed size, and as evidence what it holds,
/// so the user can see what emptying deletes. One walk measures both.
fn trash_item(root: PathBuf, ctx: &ScanContext<'_>) -> CleanItem {
    let (size, sizes) = size_by_entry(&root, ctx);
    CleanItem {
        category_id: ID.to_string(),
        category_label: LABEL.to_string(),
        evidence: contents(&root, &sizes),
        path: root,
        size,
        risk: RiskLevel::Destructive,
    }
}

/// The freed bytes of `root` (the cleanup rule) and the allocated bytes of
/// each top-level entry, from the same walk. An entry with no regular file
/// under it is absent.
fn size_by_entry(root: &Path, ctx: &ScanContext<'_>) -> (u64, HashMap<PathBuf, u64>) {
    let mut entries: HashMap<PathBuf, u64> = HashMap::new();
    let mut freed = FreedBytes::default();
    walk_files(root, ctx, &mut |path, meta| {
        freed.add_file(path, meta);
        let first = path
            .strip_prefix(root)
            .ok()
            .and_then(|rest| rest.components().next());
        if let Some(first) = first {
            let bytes = entries.entry(root.join(first)).or_default();
            *bytes = bytes.saturating_add(crate::space_lens::allocated(meta));
        }
    });
    (finish_freed(root, ctx, freed), entries)
}

/// The `LISTED` largest top-level entries, largest first (then by name),
/// and one `TrashRest` for the others. Finder's own files are left out.
fn contents(root: &Path, sizes: &HashMap<PathBuf, u64>) -> Vec<Evidence> {
    let Ok(listing) = std::fs::read_dir(root) else {
        return Vec::new();
    };
    let mut entries: Vec<(String, u64, PathBuf)> = listing
        .filter_map(|entry| entry.ok().map(|e| e.path()))
        .filter_map(|path| {
            let name = path.file_name()?.to_string_lossy().into_owned();
            (!FINDER_FILES.contains(&name.as_str()))
                .then(|| (name, sizes.get(&path).copied().unwrap_or(0), path))
        })
        .collect();
    entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    let rest = entries.split_off(entries.len().min(LISTED));
    let mut evidence: Vec<Evidence> = entries
        .into_iter()
        .map(|(name, bytes, path)| Evidence::TrashEntry {
            name,
            bytes,
            added_at: added_at(&path),
        })
        .collect();
    if !rest.is_empty() {
        evidence.push(Evidence::TrashRest {
            count: rest.len() as u64,
            bytes: rest.iter().map(|e| e.1).fold(0, u64::saturating_add),
        });
    }
    evidence
}

/// When the item arrived in its folder (APFS's date added), in Unix
/// seconds; `None` when the volume does not record it. Spotlight's
/// `kMDItemDateAdded` is null inside the Trash, so the attribute is read
/// directly.
#[cfg(target_os = "macos")]
fn added_at(path: &Path) -> Option<i64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    #[repr(C, align(8))]
    struct Reply([u8; 48]);
    // A `u32` length, the returned `attribute_set_t` (five `u32`, common
    // first), then the date as a `timespec`.
    const RETURNED_COMMON: usize = 4;
    const VALUE: usize = 24;

    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let mut request = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: libc::ATTR_CMN_RETURNED_ATTRS | libc::ATTR_CMN_ADDEDTIME,
        volattr: 0,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut reply = Reply([0; 48]);
    // SAFETY: `path` is NUL-terminated and both buffers outlive the call,
    // which writes at most `reply.0.len()` bytes. `FSOPT_NOFOLLOW` reads a
    // final symlink itself, never its target.
    let rc = unsafe {
        libc::getattrlist(
            path.as_ptr(),
            (&mut request as *mut libc::attrlist).cast(),
            reply.0.as_mut_ptr().cast(),
            reply.0.len(),
            libc::FSOPT_NOFOLLOW,
        )
    };
    if rc != 0 {
        return None;
    }
    let returned = u32::from_ne_bytes(
        reply.0[RETURNED_COMMON..RETURNED_COMMON + 4]
            .try_into()
            .ok()?,
    );
    if returned & libc::ATTR_CMN_ADDEDTIME == 0 {
        return None;
    }
    let seconds = i64::from_ne_bytes(reply.0[VALUE..VALUE + 8].try_into().ok()?);
    (seconds > 0).then_some(seconds)
}

#[cfg(not(target_os = "macos"))]
fn added_at(_: &Path) -> Option<i64> {
    None
}

fn empty_trash() -> Result<ExecReport> {
    let script = "tell application \"Finder\" to empty trash";
    let output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|e| engine_error!("failed to spawn osascript: {}", e))?;
    let mut report = ExecReport::default();
    if output.status.success() {
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            report.removed_paths.push(home.join(".Trash"));
        }
        Ok(report)
    } else {
        let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(engine_error!("Finder empty trash failed: {}", err))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_trash_action() {
        let p = TrashProvider;
        let err = p.execute(&[], ExecAction::Trash).unwrap_err();
        assert!(err.to_string().contains("rejects ExecAction::Trash"));
    }

    #[test]
    fn rejects_hard_delete() {
        let p = TrashProvider;
        let err = p.execute(&[], ExecAction::HardDelete).unwrap_err();
        assert!(err.to_string().contains("rejects ExecAction::HardDelete"));
    }

    fn write(path: &Path, bytes: usize) {
        std::fs::write(path, vec![0xA5; bytes]).unwrap();
    }

    fn listed(item: &CleanItem) -> Vec<(&str, u64)> {
        item.evidence
            .iter()
            .filter_map(|e| match e {
                Evidence::TrashEntry { name, bytes, .. } => Some((name.as_str(), *bytes)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn names_the_largest_items_and_sums_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".Trash");
        std::fs::create_dir(&root).unwrap();
        for (i, kb) in [64, 512, 8, 256, 128, 32, 16].iter().enumerate() {
            write(&root.join(format!("file-{i}")), kb * 1024);
        }
        std::fs::create_dir(root.join("folder")).unwrap();
        write(&root.join("folder").join("inside"), 1024 * 1024);
        write(&root.join(".DS_Store"), 6 * 1024);

        let item = trash_item(root.clone(), &ScanContext::unchecked());
        let names: Vec<&str> = listed(&item).iter().map(|(n, _)| *n).collect();
        assert_eq!(names, ["folder", "file-1", "file-3", "file-4", "file-0"]);
        let bytes: Vec<u64> = listed(&item).iter().map(|(_, b)| *b).collect();
        assert!(
            bytes.windows(2).all(|w| w[0] >= w[1]),
            "largest first: {bytes:?}"
        );
        let rest = item.evidence.iter().find_map(|e| match e {
            Evidence::TrashRest { count, bytes } => Some((*count, *bytes)),
            _ => None,
        });
        let small: u64 = ["file-5", "file-6", "file-2"]
            .iter()
            .map(|n| {
                crate::space_lens::allocated(&std::fs::symlink_metadata(root.join(n)).unwrap())
            })
            .sum();
        assert_eq!(rest, Some((3, small)));
        // The figure stays the whole folder's, Finder's own file included.
        assert_eq!(
            item.size,
            crate::clean::fs_safe::dir_size_checked(&root, &ScanContext::unchecked())
        );
    }

    #[test]
    fn a_short_trash_has_no_rest() {
        let dir = tempfile::tempdir().unwrap();
        write(&dir.path().join("only"), 4096);
        let item = trash_item(dir.path().to_path_buf(), &ScanContext::unchecked());
        assert_eq!(listed(&item).len(), 1);
        assert!(!item
            .evidence
            .iter()
            .any(|e| matches!(e, Evidence::TrashRest { .. })));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn reads_when_an_item_was_added_to_its_folder() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("moved-in");
        write(&file, 4096);
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64;
        let added = added_at(&file).expect("APFS records the date added");
        assert!((now - added).abs() < 120, "added {added}, now {now}");
        assert_eq!(added_at(&dir.path().join("missing")), None);
    }
}
