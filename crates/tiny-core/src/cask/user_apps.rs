//! Apps in a folder other than `/Applications`, for the question "is this
//! catalogue app already on the Mac".
//!
//! The Uninstall listing reads only the top level of `/Applications`, and
//! Uninstall must stay that way. A cask installs into `/Applications`, but the
//! same app can be in `~/Applications` (Homebrew's `--appdir`, or dragged
//! there), and `brew install` would put a second copy beside it. This reads
//! the top level of one more folder, with the same `AppEntry` shape, without
//! touching what Uninstall lists. Subfolders are not searched.

use std::fs;
use std::path::Path;
use std::time::Duration;

use crate::runner::CommandRunner;
use crate::uninstall::AppEntry;

/// Absolute so a Finder-launched app does not depend on its minimal `PATH`.
const DEFAULTS: &str = "/usr/bin/defaults";
const READ_TIMEOUT: Duration = Duration::from_secs(5);

/// The `.app` bundles directly inside `dir`, with the identifier each declares
/// (`None` when it cannot be read, which matches nothing). A folder that
/// cannot be read has none. Types are read without following a link, and a
/// link to a bundle counts as one: `brew` would still find the app there.
pub fn list_apps_in(dir: &Path, runner: &dyn CommandRunner) -> Vec<AppEntry> {
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    let mut apps: Vec<AppEntry> = entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|e| e == "app"))
        .filter(|entry| {
            entry
                .file_type()
                .is_ok_and(|kind| kind.is_dir() || kind.is_symlink())
        })
        .map(|entry| {
            let path = entry.path();
            AppEntry {
                name: path
                    .file_stem()
                    .map_or_else(String::new, |stem| stem.to_string_lossy().into_owned()),
                bundle_id: bundle_id(&path, runner),
                path,
                size: 0,
                last_used_days: None,
            }
        })
        .collect();
    apps.sort_by(|a, b| a.path.cmp(&b.path));
    apps
}

fn bundle_id(app: &Path, runner: &dyn CommandRunner) -> Option<String> {
    let info = app.join("Contents/Info");
    let outcome = runner
        .output(
            DEFAULTS,
            &["read", info.to_str()?, "CFBundleIdentifier"],
            READ_TIMEOUT,
        )
        .ok()?;
    let id = outcome.stdout.trim();
    (outcome.success() && !id.is_empty()).then(|| id.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::test_support::MockRunner;

    fn defaults_reply(runner: MockRunner, app: &Path, status: i32, stdout: &str) -> MockRunner {
        let info = app.join("Contents/Info");
        runner.with_exit(
            DEFAULTS,
            &["read", info.to_str().unwrap(), "CFBundleIdentifier"],
            status,
            stdout,
        )
    }

    #[test]
    fn the_top_level_bundles_are_listed_with_their_identifiers() {
        let dir = tempfile::tempdir().unwrap();
        for name in ["Ghostty.app", "Broken.app", "Nested"] {
            fs::create_dir(dir.path().join(name)).unwrap();
        }
        fs::create_dir(dir.path().join("Nested/Inner.app")).unwrap();
        fs::write(dir.path().join("Notes.app"), "a file, not a bundle").unwrap();
        fs::write(dir.path().join("readme.txt"), "x").unwrap();
        let runner = defaults_reply(
            defaults_reply(
                MockRunner::new(),
                &dir.path().join("Ghostty.app"),
                0,
                "com.mitchellh.ghostty\n",
            ),
            &dir.path().join("Broken.app"),
            1,
            "",
        );
        let apps = list_apps_in(dir.path(), &runner);
        let seen: Vec<_> = apps
            .iter()
            .map(|a| (a.name.as_str(), a.bundle_id.as_deref()))
            .collect();
        assert_eq!(
            seen,
            [("Broken", None), ("Ghostty", Some("com.mitchellh.ghostty"))],
            "no files, no subfolders' bundles"
        );
        assert_eq!(apps[1].path, dir.path().join("Ghostty.app"));
    }

    #[test]
    fn a_link_to_a_bundle_counts_and_a_missing_folder_has_no_apps() {
        let dir = tempfile::tempdir().unwrap();
        let real = dir.path().join("real/Rectangle.app");
        fs::create_dir_all(&real).unwrap();
        let apps_dir = dir.path().join("Applications");
        fs::create_dir(&apps_dir).unwrap();
        std::os::unix::fs::symlink(&real, apps_dir.join("Rectangle.app")).unwrap();
        let runner = defaults_reply(
            MockRunner::new(),
            &apps_dir.join("Rectangle.app"),
            0,
            "com.knollsoft.Rectangle",
        );
        let apps = list_apps_in(&apps_dir, &runner);
        assert_eq!(apps.len(), 1);
        assert_eq!(
            apps[0].bundle_id.as_deref(),
            Some("com.knollsoft.Rectangle")
        );
        assert!(list_apps_in(&dir.path().join("missing"), &MockRunner::new()).is_empty());
    }
}
