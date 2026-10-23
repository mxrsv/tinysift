//! A reviewed selection of discovered paths for the desktop's checked
//! cleanup: what each path looked like at discovery, how it will be removed,
//! and which selected paths overlap.

use std::path::{Path, PathBuf};

use super::freed_size::{union_bytes, SharedBlocks};
use super::fs_safe::PathFingerprint;
use super::providers::{category_section, Section};
use super::types::CleanItem;

/// One selected path with the trusted discovery-time data execution checks
/// it against.
#[derive(Debug, Clone)]
pub struct PlannedItem {
    pub item: CleanItem,
    pub fingerprint: PathFingerprint,
    /// Directories discovery listed or walked for the item's category;
    /// execution acts only on paths inside one of them.
    pub roots: Vec<PathBuf>,
    /// Selected items merged into this one (the same path, or paths inside
    /// it). Moving this item moves them too, so their providers' guards and
    /// app gates apply as well.
    pub covers: Vec<CleanItem>,
    /// How execution removes the path, fixed when the selection is reviewed.
    pub removal: Removal,
    /// `item.size` counts blocks the volume could not show as unshared.
    pub size_is_upper_bound: bool,
    /// Blocks the item shares with files outside it, for totals over
    /// several items ([`freed_together`]).
    pub shared: SharedBlocks,
}

/// How the desktop removes one planned path.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum Removal {
    /// Through Finder, so Put Back works.
    #[default]
    MoveToTrash,
    /// For good, through `fs_safe`'s guarded removal.
    Delete,
    /// Finder's Empty Trash: only the `trash` row (`desktop_empties_trash`).
    EmptyTrash,
}

/// The app's setting for what comes back on its own (OC1). `Trash` is how
/// the app always cleaned before the setting existed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum DeleteMode {
    #[default]
    Trash,
    Permanent,
}

/// `Delete` when `mode` is permanent and the item, and everything it would
/// carry along, sits in Rebuilt automatically or Project build output;
/// otherwise `MoveToTrash`, so Your files and unregistered categories always
/// go to the Trash (OC1, OC2). Merging never widens: a cache folder holding
/// a path that only goes to the Trash goes to the Trash with it. The Trash
/// row's `EmptyTrash` comes from its provider, not from here.
pub fn removal_for(item: &CleanItem, covers: &[CleanItem], mode: DeleteMode) -> Removal {
    let rebuilt = |item: &CleanItem| {
        matches!(
            category_section(&item.category_id),
            Some(Section::Rebuilt | Section::BuildOutput)
        )
    };
    if mode == DeleteMode::Permanent && std::iter::once(item).chain(covers).all(rebuilt) {
        Removal::Delete
    } else {
        Removal::MoveToTrash
    }
}

/// The bytes removing `items` together frees: their sizes, plus the blocks
/// they share only with each other, counted once. True when that is only an
/// upper bound. The items must not overlap (`partition_overlaps`).
pub fn freed_together<'a>(items: impl IntoIterator<Item = &'a PlannedItem>) -> (u64, bool) {
    let items: Vec<&PlannedItem> = items.into_iter().collect();
    let sizes = items
        .iter()
        .fold(0u64, |sum, planned| sum.saturating_add(planned.item.size));
    let (shared, upper_bound) = union_bytes(items.iter().map(|planned| &planned.shared));
    let upper_bound = upper_bound || items.iter().any(|planned| planned.size_is_upper_bound);
    (sizes.saturating_add(shared), upper_bound)
}

/// Why `path` must never be cleaned, or `None`. The file-system root, the
/// home folder and its ancestors are always refused. A path that came from
/// a tool's output (`npm config get cache`, `go env GOCACHE`, ...) must
/// also lie strictly inside the home folder, so a misconfigured tool
/// cannot point cleanup at arbitrary data.
pub fn protected_reason(path: &Path, home: Option<&Path>, tool_printed: bool) -> Option<String> {
    if path.parent().is_none() {
        return Some("the file-system root".into());
    }
    let Some(home) = home else {
        return tool_printed.then(|| "the home folder is unknown".into());
    };
    if path == home {
        Some("the home folder".into())
    } else if home.starts_with(path) {
        Some("an ancestor of the home folder".into())
    } else if tool_printed && !path.starts_with(home) {
        Some("a tool reported a path outside the home folder".into())
    } else {
        None
    }
}

/// True when `path` is inside `/System`, compared without letter case as
/// APFS usually does, or holds a `..` that could lead there. Cleanup never
/// deletes or moves anything there, even a path a provider listed.
pub fn inside_system(path: &Path) -> bool {
    use std::os::unix::ffi::OsStrExt;
    use std::path::Component;
    if path.components().any(|c| c == Component::ParentDir) {
        return true;
    }
    let mut components = path.components();
    components.next() == Some(Component::RootDir)
        && matches!(components.next(), Some(Component::Normal(first))
            if first.as_bytes().eq_ignore_ascii_case(b"System"))
}

/// The current account's home folder from the user database
/// (`getpwuid_r(getuid())`), independent of the `HOME` variable.
pub fn account_home() -> Option<PathBuf> {
    use std::ffi::{CStr, OsStr};
    use std::os::unix::ffi::OsStrExt;
    let mut len = 4096;
    while len <= 1 << 20 {
        let mut buf = vec![0 as libc::c_char; len];
        // SAFETY: zeroed `passwd` is a valid out-parameter; `buf` outlives
        // every read of the strings it backs.
        let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
        let mut found: *mut libc::passwd = std::ptr::null_mut();
        let rc = unsafe {
            libc::getpwuid_r(libc::getuid(), &mut pwd, buf.as_mut_ptr(), len, &mut found)
        };
        if rc == libc::ERANGE {
            len *= 2;
            continue;
        }
        if rc != 0 || found.is_null() || pwd.pw_dir.is_null() {
            return None;
        }
        let dir = unsafe { CStr::from_ptr(pwd.pw_dir) };
        return Some(PathBuf::from(OsStr::from_bytes(dir.to_bytes())));
    }
    None
}

/// The home folder the desktop may trust: `HOME` (normalized) when it is
/// absolute, not `/`, free of `..`, and equal to the account's home.
pub fn trusted_home(env_home: Option<&Path>, account: Option<&Path>) -> Result<PathBuf, String> {
    let normalize = |path: &Path, what: &str| -> Result<PathBuf, String> {
        if !path.is_absolute() {
            return Err(format!("{what} is not an absolute path"));
        }
        if path
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(format!("{what} contains '..'"));
        }
        Ok(path.components().collect())
    };
    let env_home = env_home
        .filter(|h| !h.as_os_str().is_empty())
        .ok_or("HOME is not set")?;
    let home = normalize(env_home, "HOME")?;
    if home.parent().is_none() {
        return Err("HOME is /".into());
    }
    let account = account.ok_or("the account home could not be read")?;
    if normalize(account, "the account home")? != home {
        return Err("HOME does not match the account home".into());
    }
    Ok(home)
}

/// Why a selected path is left out of the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Overlap {
    /// The same path is already selected (another category found it too).
    Duplicate { kept: usize },
    /// An ancestor is already selected and moves this path with it.
    Inside { kept: usize },
}

/// Splits `entries` into the paths to act on (sorted by path) and the ones
/// another kept entry already covers; `kept` indexes the first list. This
/// keeps one path from being moved twice and its bytes from being counted
/// twice.
pub fn partition_overlaps<T>(
    mut entries: Vec<T>,
    path: impl Fn(&T) -> &Path,
) -> (Vec<T>, Vec<(T, Overlap)>) {
    // Component order puts every descendant right after its ancestor.
    entries.sort_by(|a, b| path(a).cmp(path(b)));
    let mut kept: Vec<T> = Vec::new();
    let mut covered = Vec::new();
    for entry in entries {
        let overlap = kept.last().and_then(|last| {
            let index = kept.len() - 1;
            if path(&entry) == path(last) {
                Some(Overlap::Duplicate { kept: index })
            } else if path(&entry).starts_with(path(last)) {
                Some(Overlap::Inside { kept: index })
            } else {
                None
            }
        });
        match overlap {
            Some(overlap) => covered.push((entry, overlap)),
            None => kept.push(entry),
        }
    }
    (kept, covered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn split(paths: &[&str]) -> (Vec<PathBuf>, Vec<(PathBuf, Overlap)>) {
        let entries = paths.iter().map(PathBuf::from).collect();
        partition_overlaps(entries, |p: &PathBuf| p.as_path())
    }

    #[test]
    fn duplicates_and_descendants_are_covered_by_the_kept_path() {
        let (kept, covered) = split(&[
            "/c/Caches/com.apple.Safari",
            "/c/Caches",
            "/c/Caches-other",
            "/c/Caches/go-build",
            "/c/Caches",
        ]);
        assert_eq!(
            kept,
            vec![PathBuf::from("/c/Caches"), PathBuf::from("/c/Caches-other")]
        );
        assert_eq!(
            covered,
            vec![
                (PathBuf::from("/c/Caches"), Overlap::Duplicate { kept: 0 }),
                (
                    PathBuf::from("/c/Caches/com.apple.Safari"),
                    Overlap::Inside { kept: 0 }
                ),
                (
                    PathBuf::from("/c/Caches/go-build"),
                    Overlap::Inside { kept: 0 }
                ),
            ]
        );
    }

    #[test]
    fn root_home_and_its_ancestors_are_always_protected() {
        let home = Path::new("/Users/me");
        let check = |p: &str, tool| protected_reason(Path::new(p), Some(home), tool);
        assert!(check("/", false).is_some());
        assert!(check("/Users/me", false).is_some());
        assert!(check("/Users", false).is_some());
        assert!(check("/Users/me/.npm", false).is_none());
        assert!(check("/Library/Logs/DiagnosticReports", false).is_none());
        assert!(check("/Library/Logs/DiagnosticReports", true).is_some());
        assert!(check("/Users/me/.npm", true).is_none());
        assert!(check("/Users/meow/.npm", true).is_some(), "name prefix");
        assert!(protected_reason(Path::new("/x/y"), None, true).is_some());
        assert!(protected_reason(Path::new("/x/y"), None, false).is_none());
    }

    #[test]
    fn system_paths_are_recognized_in_any_case_and_through_dot_dot() {
        for inside in [
            "/System",
            "/System/Library/AssetsV2/x.dmg",
            "/system/library/assetsv2/x.dmg",
            "/SYSTEM/Library",
            "//System/Library",
            "/System/Volumes/Data/System/Library/AssetsV2/x.dmg",
            "/Library/../System/Library/AssetsV2/x.dmg",
            "/Library/Developer/CoreSimulator/Caches/..",
        ] {
            assert!(inside_system(Path::new(inside)), "{inside}");
        }
        for outside in [
            "/Library/Developer/CoreSimulator/Caches/dyld",
            "/Users/me/System/x",
            "/Systems/x",
            "/Sys",
            "<simulator-runtime:1732E568-CE24-44BA-B87A-12934CCCC76F>",
        ] {
            assert!(!inside_system(Path::new(outside)), "{outside}");
        }
    }

    #[test]
    fn home_is_trusted_only_when_it_matches_the_account() {
        let me = Some(Path::new("/Users/me"));
        let trust = |env: &str| trusted_home(Some(Path::new(env)), me);
        assert_eq!(trust("/Users/me/"), Ok(PathBuf::from("/Users/me")));
        assert_eq!(trust("/Users/me/./"), Ok(PathBuf::from("/Users/me")));
        assert!(trust("/Users/other").is_err());
        assert!(trust("/").is_err());
        assert!(trust("Users/me").is_err());
        assert!(trust("/Users/other/../me").is_err());
        assert!(trust("").is_err());
        assert!(trusted_home(None, me).is_err());
        assert!(trusted_home(Some(Path::new("/Users/me")), None).is_err());
    }

    #[test]
    fn the_account_home_is_an_absolute_path() {
        let home = account_home().expect("the test account has a home");
        assert!(home.is_absolute());
    }

    #[test]
    fn a_name_prefix_is_not_an_ancestor() {
        let (kept, covered) = split(&["/a/node", "/a/node_modules"]);
        assert_eq!(kept.len(), 2);
        assert!(covered.is_empty());
    }
}
