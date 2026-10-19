//! Ownership rules for uninstall leftovers (spec rules O1-O5).

use super::{AppEntry, ItemKind, RemovalItem};
use std::fs;
use std::path::{Path, PathBuf};

/// Longest bundle identifier accepted in a path.
const MAX_BUNDLE_ID_LEN: usize = 255;

const REASON_NO_INVENTORY: &str =
    "installed apps could not be listed, so a second copy of this app cannot be ruled out";
const REASON_DUPLICATE_ID: &str = "another installed app has the same bundle identifier";
const REASON_LAUNCH_AGENT: &str =
    "name only starts with the bundle id at a dot; may belong to a helper or another app";
const REASON_GROUP_CONTAINER: &str = "group containers are shared between apps";
const REASON_SYMLINK: &str = "is a symbolic link";
const REASON_PARENT: &str = "its parent folder is not a real directory";

/// How recoverable a leftover is (O2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum LeftoverClass {
    /// The app rebuilds it (`Caches`).
    Regenerable,
    /// Settings, app data, cookies, saved state, LaunchAgents.
    DataRisk,
}

/// Whether an item may be removed, and why not when it may not.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Ownership {
    /// The `.app` bundle itself.
    AppBundle,
    /// Exact-name leftover of this app (O2).
    Owned { class: LeftoverClass },
    /// Listed for information, never removed (O3, O5).
    ReportOnly { reason: String },
    /// Could not be verified safe; never removed (O5).
    Uncertain { reason: String },
}

/// Who chose the action: `-y` acts on the safe subset, the menu on everything owned.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Consent {
    /// Non-interactive (`-y`): app bundle and regenerable leftovers only.
    Unattended,
    /// The user picked the action in the interactive menu.
    Explicit,
}

impl Ownership {
    /// True when an action taken with `consent` may remove the item.
    pub fn allows(&self, consent: Consent) -> bool {
        match self {
            Self::AppBundle
            | Self::Owned {
                class: LeftoverClass::Regenerable,
            } => true,
            Self::Owned {
                class: LeftoverClass::DataRisk,
            } => consent == Consent::Explicit,
            Self::ReportOnly { .. } | Self::Uncertain { .. } => false,
        }
    }
}

/// True when `id` is safe to join into a `~/Library` path: reverse-DNS with at
/// least two non-empty dot-separated components of ASCII letters, digits or `-`.
/// This excludes `/`, `..`, `.`, whitespace and control characters by construction.
pub fn valid_bundle_id(id: &str) -> bool {
    id.len() <= MAX_BUNDLE_ID_LEN
        && id.split('.').count() >= 2
        && id.split('.').all(|part| {
            !part.is_empty() && part.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}

/// A launch agent `<id>.<suffix>.plist`; the exact `<id>.plist` is an O2 entry.
fn launch_agent_boundary(name: &str, id: &str) -> bool {
    name.strip_suffix(".plist")
        .and_then(|stem| stem.strip_prefix(id))
        .is_some_and(|rest| rest.len() > 1 && rest.starts_with('.'))
}

/// A group container named with `id` only at a `.` boundary (O3).
fn group_container_boundary(name: &str, id: &str) -> bool {
    name == id
        || name.ends_with(&format!(".{id}"))
        || name.starts_with(&format!("{id}."))
        || name.contains(&format!(".{id}."))
}

/// Why every leftover must be report-only, if it must (O5): `inventory` is
/// unreadable (`None`) or lists another app with the same bundle id.
pub fn doubt_reason(
    app_path: &Path,
    id: &str,
    inventory: Option<&[AppEntry]>,
) -> Option<&'static str> {
    let Some(apps) = inventory else {
        return Some(REASON_NO_INVENTORY);
    };
    // Library folders resolve case-insensitively on the default volume, so
    // `com.foo.Bar` and `com.foo.bar` share every leftover.
    apps.iter()
        .any(|a| {
            !same_entry(&a.path, app_path)
                && a.bundle_id
                    .as_deref()
                    .is_some_and(|other| other.eq_ignore_ascii_case(id))
        })
        .then_some(REASON_DUPLICATE_ID)
}

/// The same directory entry, even when spelled differently (`claude.app` and
/// `Claude.app` on a case-insensitive volume). Unreadable means "not the same".
fn same_entry(a: &Path, b: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    a == b
        || matches!(
            (fs::symlink_metadata(a), fs::symlink_metadata(b)),
            (Ok(x), Ok(y)) if x.dev() == y.dev() && x.ino() == y.ino()
        )
}

enum Match {
    Exact(LeftoverClass),
    Boundary(&'static str),
}

fn exact_candidates(library: &Path, id: &str) -> Vec<(PathBuf, Match)> {
    use LeftoverClass::{DataRisk, Regenerable};
    let data = |dir: &str, name: String| (library.join(dir).join(name), Match::Exact(DataRisk));
    let mut out = vec![(library.join("Caches").join(id), Match::Exact(Regenerable))];
    for dir in [
        "Application Support",
        "Containers",
        "HTTPStorages",
        "WebKit",
    ] {
        out.push(data(dir, id.to_string()));
    }
    out.push(data("Preferences", format!("{id}.plist")));
    out.push(data("Saved Application State", format!("{id}.savedState")));
    out.push(data("HTTPStorages", format!("{id}.binarycookies")));
    out.push(data("LaunchAgents", format!("{id}.plist")));
    out
}

fn boundary_candidates(library: &Path, id: &str) -> Vec<(PathBuf, Match)> {
    let agents = matching_entries(&library.join("LaunchAgents"), |n| {
        launch_agent_boundary(n, id)
    });
    let groups = matching_entries(&library.join("Group Containers"), |n| {
        group_container_boundary(n, id)
    });
    agents
        .into_iter()
        .map(|p| (p, Match::Boundary(REASON_LAUNCH_AGENT)))
        .chain(
            groups
                .into_iter()
                .map(|p| (p, Match::Boundary(REASON_GROUP_CONTAINER))),
        )
        .collect()
}

fn matching_entries(dir: &Path, keep: impl Fn(&str) -> bool) -> Vec<PathBuf> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|s| s.to_str()).is_some_and(&keep))
        .collect()
}

/// Lists the leftovers of the app, each tagged with its ownership. Nothing is
/// listed without a valid bundle id (O1); display-name paths, shared roots,
/// dot folders and system `/Library` are never candidates (O4).
pub(crate) fn find_leftovers(
    home: &Path,
    bundle_id: Option<&str>,
    inventory: Option<&[AppEntry]>,
    app_path: &Path,
) -> Vec<RemovalItem> {
    let Some(id) = bundle_id.filter(|b| valid_bundle_id(b)) else {
        return Vec::new();
    };
    let doubt = doubt_reason(app_path, id, inventory);
    let library = home.join("Library");
    let mut candidates = exact_candidates(&library, id);
    candidates.extend(boundary_candidates(&library, id));
    candidates
        .into_iter()
        .filter_map(|(path, m)| leftover_item(home, &library, path, m, doubt))
        .collect()
}

fn is_real_dir(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
}

/// Builds the item for an existing candidate strictly inside `library` that is
/// not protected. Symlinks and symlinked ancestors are listed as uncertain.
fn leftover_item(
    home: &Path,
    library: &Path,
    path: PathBuf,
    m: Match,
    doubt: Option<&str>,
) -> Option<RemovalItem> {
    if path == library || !path.starts_with(library) {
        return None;
    }
    if crate::clean::trash_plan::protected_reason(&path, Some(home), false).is_some() {
        return None;
    }
    let meta = fs::symlink_metadata(&path).ok()?;
    let parent_ok = path.parent().is_some_and(is_real_dir) && is_real_dir(library);
    let ownership = if meta.file_type().is_symlink() {
        Ownership::Uncertain {
            reason: REASON_SYMLINK.into(),
        }
    } else if !parent_ok {
        Ownership::Uncertain {
            reason: REASON_PARENT.into(),
        }
    } else {
        match (m, doubt) {
            (Match::Boundary(reason), _) | (Match::Exact(_), Some(reason)) => {
                Ownership::ReportOnly {
                    reason: reason.into(),
                }
            }
            (Match::Exact(class), None) => Ownership::Owned { class },
        }
    };
    let size = if meta.is_dir() {
        crate::clean::fs_safe::dir_size_safe(&path)
    } else {
        meta.len()
    };
    Some(RemovalItem {
        path,
        size,
        kind: ItemKind::Leftover,
        ownership,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_unsafe_or_malformed_ids() {
        let too_long = format!("com.{}", "a".repeat(MAX_BUNDLE_ID_LEN));
        for id in [
            ".",
            "..",
            "../..",
            "a/b",
            "",
            "com",
            "com..x",
            "com.",
            ".com",
            "com. x",
            "com.ex\u{7}ample",
            "com.ex\nample",
            "com.exämple",
            "com.my_app",
            too_long.as_str(),
        ] {
            assert!(!valid_bundle_id(id), "{id:?} must be rejected");
        }
    }

    #[test]
    fn accepts_reverse_dns_ids() {
        for id in ["com.example.App", "com.example.my-app", "org.x2.y"] {
            assert!(valid_bundle_id(id), "{id:?} must be accepted");
        }
    }

    const ID: &str = "com.example.App";

    fn app_path() -> PathBuf {
        PathBuf::from("/Applications/App.app")
    }

    fn entry(path: &str, id: Option<&str>) -> AppEntry {
        AppEntry {
            name: "x".into(),
            path: PathBuf::from(path),
            bundle_id: id.map(String::from),
            size: 0,
            last_used_days: None,
        }
    }

    fn lib_home(dirs: &[&str]) -> tempfile::TempDir {
        let home = tempfile::tempdir().unwrap();
        for d in dirs {
            fs::create_dir_all(home.path().join("Library").join(d)).unwrap();
        }
        home
    }

    fn touch(home: &Path, rel: &str) {
        let p = home.join("Library").join(rel);
        fs::create_dir_all(p.parent().unwrap()).unwrap();
        fs::write(p, b"x").unwrap();
    }

    fn scan(home: &Path, inv: Option<&[AppEntry]>) -> Vec<RemovalItem> {
        find_leftovers(home, Some(ID), inv, &app_path())
    }

    #[test]
    fn a_twin_differing_only_in_case_is_a_duplicate() {
        let other = [entry("/Applications/Other.app", Some("COM.example.app"))];
        assert_eq!(
            doubt_reason(&app_path(), ID, Some(&other)),
            Some(REASON_DUPLICATE_ID)
        );
    }

    #[test]
    fn the_same_app_spelled_differently_is_not_its_own_twin() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("App.app");
        fs::create_dir_all(dir.path().join("sub")).unwrap();
        fs::create_dir(&app).unwrap();
        let listed = dir.path().join("sub/../App.app");
        let inventory = [entry(listed.to_str().unwrap(), Some(ID))];
        assert_eq!(doubt_reason(&app, ID, Some(&inventory)), None);
        let missing = [entry("/nowhere/App.app", Some(ID))];
        assert_eq!(
            doubt_reason(&app, ID, Some(&missing)),
            Some(REASON_DUPLICATE_ID),
            "an entry that cannot be read counts as another copy"
        );
    }

    #[test]
    fn exact_names_are_classified() {
        let home = lib_home(&[]);
        for rel in [
            "Caches/com.example.App/f",
            "Application Support/com.example.App/f",
            "Containers/com.example.App/f",
            "HTTPStorages/com.example.App/f",
            "WebKit/com.example.App/f",
            "Preferences/com.example.App.plist",
            "Saved Application State/com.example.App.savedState/f",
            "HTTPStorages/com.example.App.binarycookies",
            "LaunchAgents/com.example.App.plist",
            "Logs/com.example.App/f",
        ] {
            touch(home.path(), rel);
        }
        let items = scan(home.path(), Some(&[]));
        assert_eq!(items.len(), 9, "Logs must not be listed");
        for i in &items {
            let cache = i.path.parent().unwrap().ends_with("Caches");
            let want = if cache {
                LeftoverClass::Regenerable
            } else {
                LeftoverClass::DataRisk
            };
            assert_eq!(
                i.ownership,
                Ownership::Owned { class: want },
                "{:?}",
                i.path
            );
        }
    }

    #[test]
    fn launch_agent_boundary_only_at_dot() {
        assert!(launch_agent_boundary("com.example.App.helper.plist", ID));
        for n in [
            "com.example.App.plist",
            "com.example.AppHelper.plist",
            "com.example.App.",
            "com.example.App..plist",
            "xcom.example.App.helper.plist",
            "com.example.App.helper",
        ] {
            assert!(!launch_agent_boundary(n, ID), "{n}");
        }
    }

    #[test]
    fn group_container_boundaries() {
        for n in [
            "com.example.App",
            "group.com.example.App",
            "TEAM.com.example.App",
            "com.example.App.shared",
            "x.com.example.App.y",
        ] {
            assert!(group_container_boundary(n, ID), "{n}");
        }
        for n in [
            "com.example.AppHelper",
            "com.example.AppX",
            "xcom.example.App",
            "com.example.Ap",
        ] {
            assert!(!group_container_boundary(n, ID), "{n}");
        }
    }

    #[test]
    fn boundary_matches_are_report_only_and_lookalikes_unlisted() {
        let home = lib_home(&["Group Containers", "LaunchAgents"]);
        for rel in [
            "Group Containers/group.com.example.App/f",
            "Group Containers/com.example.AppHelper/f",
            "LaunchAgents/com.example.App.helper.plist",
            "LaunchAgents/com.example.AppX.plist",
        ] {
            touch(home.path(), rel);
        }
        let items = scan(home.path(), Some(&[]));
        assert_eq!(items.len(), 2);
        assert!(items
            .iter()
            .all(|i| matches!(i.ownership, Ownership::ReportOnly { .. })));
    }

    #[test]
    fn display_name_and_dot_folders_are_ignored() {
        let home = lib_home(&["Application Support/App", "Caches/App"]);
        fs::create_dir_all(home.path().join(".App")).unwrap();
        assert!(find_leftovers(home.path(), Some(ID), Some(&[]), &app_path()).is_empty());
    }

    #[test]
    fn doubt_makes_exact_leftovers_report_only() {
        let home = lib_home(&["Caches/com.example.App"]);
        let twin = [entry("/Applications/Other.app", Some(ID))];
        for inv in [None, Some(&twin[..])] {
            let items = scan(home.path(), inv);
            assert_eq!(items.len(), 1);
            assert!(matches!(items[0].ownership, Ownership::ReportOnly { .. }));
        }
        let same_app = [entry("/Applications/App.app", Some(ID))];
        let items = scan(home.path(), Some(&same_app));
        assert!(matches!(items[0].ownership, Ownership::Owned { .. }));
    }

    #[test]
    fn symlink_candidate_is_uncertain() {
        let home = lib_home(&["Caches"]);
        let target = home.path().join("elsewhere");
        fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, home.path().join("Library/Caches/com.example.App"))
            .unwrap();
        let items = scan(home.path(), Some(&[]));
        assert_eq!(items.len(), 1);
        assert!(matches!(items[0].ownership, Ownership::Uncertain { .. }));
    }

    #[test]
    fn symlinked_parent_is_uncertain() {
        let home = lib_home(&[]);
        let real = home.path().join("real-caches");
        fs::create_dir_all(real.join(ID)).unwrap();
        fs::create_dir_all(home.path().join("Library")).unwrap();
        std::os::unix::fs::symlink(&real, home.path().join("Library/Caches")).unwrap();
        let items = scan(home.path(), Some(&[]));
        assert_eq!(items.len(), 1);
        assert!(matches!(items[0].ownership, Ownership::Uncertain { .. }));
    }

    #[test]
    fn invalid_or_missing_id_lists_nothing() {
        let home = lib_home(&["Caches/com.example.App"]);
        for id in [Some("../.."), Some("."), Some(".."), Some("a/b"), None] {
            assert!(find_leftovers(home.path(), id, Some(&[]), &app_path()).is_empty());
        }
    }

    #[test]
    fn consent_filter_gates_each_class() {
        let reason = || "r".to_string();
        let cases = [
            (Ownership::AppBundle, true, true),
            (
                Ownership::Owned {
                    class: LeftoverClass::Regenerable,
                },
                true,
                true,
            ),
            (
                Ownership::Owned {
                    class: LeftoverClass::DataRisk,
                },
                false,
                true,
            ),
            (Ownership::ReportOnly { reason: reason() }, false, false),
            (Ownership::Uncertain { reason: reason() }, false, false),
        ];
        for (o, unattended, explicit) in cases {
            assert_eq!(o.allows(Consent::Unattended), unattended, "{o:?}");
            assert_eq!(o.allows(Consent::Explicit), explicit, "{o:?}");
        }
    }
}
