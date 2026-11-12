//! Places that look like a cache but hold what nothing rebuilds on its own:
//! downloaded test browsers, models, installed dependencies, a live
//! terminal's data. Broad cache sweeps (`user-caches`, the package-manager
//! caches whose folder a tool prints) skip every entry here (SF1). A category
//! that names one deliberately (`puppeteer`, `huggingface`) is not a broad
//! sweep and does not use this list.

use std::path::{Component, Path};

use super::root_as_item;
use crate::clean::fs_safe::list_children;
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, RiskLevel};

/// One place a broad sweep must leave alone. `name` and `swept_by` document
/// the entry and drive its test.
#[allow(dead_code)]
pub(super) struct UserData {
    pub name: &'static str,
    /// Trailing path components. The entry is any path that ends with them,
    /// or lies inside such a path.
    pub suffix: &'static str,
    /// The broad sweep (a provider id) that would reach it. A test per entry
    /// runs that provider.
    pub swept_by: &'static str,
}

const USER_CACHES: &str = "user-caches";
const NPM: &str = "npm";
const YARN: &str = "yarn";

pub(super) const USER_DATA: &[UserData] = &[
    UserData {
        name: "Spotify offline songs",
        suffix: "Library/Caches/com.spotify.client",
        swept_by: USER_CACHES,
    },
    UserData {
        name: "Cypress test runner",
        suffix: "Library/Caches/Cypress",
        swept_by: USER_CACHES,
    },
    UserData {
        name: "Playwright browsers",
        suffix: "Library/Caches/ms-playwright",
        swept_by: USER_CACHES,
    },
    UserData {
        name: "Camoufox browser",
        suffix: "Library/Caches/camoufox",
        swept_by: USER_CACHES,
    },
    UserData {
        name: "Buzz Whisper models and plugins",
        suffix: "Library/Caches/Buzz",
        swept_by: USER_CACHES,
    },
    UserData {
        name: "llama.cpp models",
        suffix: "Library/Caches/llama.cpp",
        swept_by: USER_CACHES,
    },
    UserData {
        name: "Kitty terminal data",
        suffix: "Library/Caches/kitty",
        swept_by: USER_CACHES,
    },
    UserData {
        name: "Poetry virtualenvs",
        suffix: "Library/Caches/pypoetry/virtualenvs",
        swept_by: USER_CACHES,
    },
    UserData {
        name: "renv package library",
        suffix: "Library/Caches/org.R-project.R/R/renv",
        swept_by: USER_CACHES,
    },
    UserData {
        name: "pnpm dlx installs",
        suffix: "Library/Caches/pnpm/dlx",
        swept_by: USER_CACHES,
    },
    UserData {
        name: "npx installs",
        suffix: "_npx",
        swept_by: NPM,
    },
    UserData {
        name: "Yarn Berry dependencies",
        suffix: ".yarn/berry",
        swept_by: YARN,
    },
];

fn components(path: &Path) -> Vec<Component<'_>> {
    path.components().collect()
}

/// The entry `path` is, or lies inside.
fn inside(path: &Path) -> Option<&'static UserData> {
    USER_DATA.iter().find(|entry| {
        path.ancestors()
            .any(|ancestor| ancestor.ends_with(entry.suffix))
    })
}

/// The entry that lies inside `path` (it exists on disk), found by
/// splitting the entry's suffix between `path`'s own tail and what is below it.
fn below(path: &Path) -> Option<&'static UserData> {
    USER_DATA.iter().find(|entry| {
        let parts = components(Path::new(entry.suffix));
        (0..parts.len()).any(|split| {
            let head: std::path::PathBuf = parts[..split].iter().collect();
            let rest: std::path::PathBuf = parts[split..].iter().collect();
            (head.as_os_str().is_empty() || path.ends_with(&head))
                && std::fs::symlink_metadata(path.join(rest)).is_ok()
        })
    })
}

/// The entry `path` is, lies inside, or holds.
pub(super) fn holds_user_data(path: &Path) -> Option<&'static UserData> {
    inside(path).or_else(|| below(path))
}

/// `root` as one item, unless user data is in the way: nothing when `root`
/// is or lies inside an entry, and its other children one by one when it
/// holds one (a child that is or holds an entry is left out too).
pub(super) fn root_sparing_user_data(
    ctx: &ScanContext<'_>,
    root: &Path,
    id: &str,
    label: &str,
    risk: RiskLevel,
) -> Vec<CleanItem> {
    if inside(root).is_some() {
        return Vec::new();
    }
    if below(root).is_none() {
        return root_as_item(ctx, root, id, label, risk);
    }
    list_children(root, ctx)
        .into_iter()
        .filter(|child| holds_user_data(child).is_none())
        .flat_map(|child| root_as_item(ctx, &child, id, label, risk))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::process::test_support::MockChecker;
    use crate::clean::providers::dev_caches::{NpmCache, YarnCache};
    use crate::clean::providers::user_caches::UserCaches;
    use crate::clean::providers::CleanProvider;
    use crate::runner::test_support::MockRunner;
    use std::sync::Arc;

    fn printed(path: &Path) -> String {
        format!("{}\n", path.display())
    }

    fn names(items: &[CleanItem]) -> Vec<String> {
        let mut names: Vec<String> = items
            .iter()
            .map(|i| i.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        names.sort();
        names
    }

    /// A fake home holding `entry` (and a plain cache beside it) as real
    /// folders; the provider that sweeps it must offer the plain one only.
    fn offered_beside(entry: &UserData) -> (Vec<String>, std::path::PathBuf) {
        let home = tempfile::tempdir().unwrap().keep();
        let held = home.join(entry.suffix);
        std::fs::create_dir_all(held.join("data")).unwrap();
        std::fs::write(held.join("data/blob"), [1u8; 64]).unwrap();
        let idle = MockChecker::none();
        let ctx = ScanContext::new(None, &idle);
        let offered = match entry.swept_by {
            USER_CACHES => {
                let root = home.join("Library/Caches");
                std::fs::create_dir_all(root.join("Homebrew")).unwrap();
                let caches = UserCaches::with_runner(Arc::new(MockRunner::new()));
                caches.list_caches(&ctx, &root).unwrap()
            }
            NPM => {
                // npm's cache folder, which npm prints, holds `_npx` beside its own.
                let cache = home.join(".npm");
                std::fs::create_dir_all(cache.join("_cacache")).unwrap();
                std::fs::rename(&held, cache.join("_npx")).unwrap();
                let runner = MockRunner::new().with_which("npm").with_exit(
                    "npm",
                    &["config", "get", "cache"],
                    0,
                    &printed(&cache),
                );
                NpmCache::with_runner(Arc::new(runner))
                    .discover(&ctx)
                    .unwrap()
            }
            YARN => {
                // Berry prints its global archive folder; the whole folder is user data.
                let runner = MockRunner::new().with_which("yarn").with_exit(
                    "yarn",
                    &["cache", "dir"],
                    0,
                    &printed(&held.join("cache")),
                );
                std::fs::create_dir_all(held.join("cache")).unwrap();
                YarnCache::with_runner(Arc::new(runner))
                    .discover(&ctx)
                    .unwrap()
            }
            other => panic!("{}: no test for the sweep `{other}`", entry.name),
        };
        (names(&offered), home)
    }

    #[test]
    fn no_sweep_offers_an_entry_of_the_list() {
        for entry in USER_DATA {
            let (offered, home) = offered_beside(entry);
            let leaf = Path::new(entry.suffix)
                .file_name()
                .unwrap()
                .to_string_lossy()
                .into_owned();
            assert!(
                !offered.contains(&leaf),
                "{}: offered {offered:?}",
                entry.name
            );
            // Not an empty list for a wrong reason: the sweep still offers the rest.
            match entry.swept_by {
                USER_CACHES => assert!(offered.contains(&"Homebrew".to_string()), "{}", entry.name),
                NPM => assert_eq!(offered, ["_cacache"], "{}", entry.name),
                _ => assert!(offered.is_empty(), "{}", entry.name),
            }
            let _ = crate::clean::fs_safe::remove_recursive_safe(&home);
        }
    }

    #[test]
    fn a_folder_holding_an_entry_is_not_offered_whole_and_a_lookalike_is() {
        let home = tempfile::tempdir().unwrap();
        let caches = home.path().join("Library/Caches");
        std::fs::create_dir_all(caches.join("pypoetry/virtualenvs")).unwrap();
        std::fs::create_dir_all(caches.join("Buzz/models")).unwrap();
        std::fs::create_dir_all(caches.join("BuzzFeed")).unwrap();
        assert!(holds_user_data(&caches.join("pypoetry")).is_some());
        assert!(holds_user_data(&caches.join("Buzz/models")).is_some());
        assert!(holds_user_data(&caches.join("BuzzFeed")).is_none());
        assert!(holds_user_data(&caches.join("pypoetry/artifacts")).is_none());
    }

    #[test]
    fn every_entry_names_a_sweep_that_the_test_runs() {
        for entry in USER_DATA {
            assert!(
                [USER_CACHES, NPM, YARN].contains(&entry.swept_by),
                "{}",
                entry.name
            );
            assert!(Path::new(entry.suffix).is_relative(), "{}", entry.name);
        }
    }
}
