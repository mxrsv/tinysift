//! Superseded Codex releases: folders in `~/.codex/packages/<package>/releases`
//! that the package's `current` link does not point at. Codex keeps every
//! release it installs. Nothing else under `~/.codex` (sessions, history,
//! databases, worktrees, credentials) is ever offered.

use std::ffi::OsStr;
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};

use crate::error::Result;

use super::{execute_per_item, root_as_item, CleanProvider};
use crate::clean::fs_safe::{is_dir_safe, list_children};
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};

const ID: &str = "codex-releases";
const LABEL: &str = "Old Codex releases";
const PACKAGES_DIR: &str = ".codex/packages";
const RELEASES: &str = "releases";
const CURRENT: &str = "current";

/// A running Codex may still load `codex-path/` and `codex-resources/` from
/// the release `current` named when it started, and its updater adds
/// releases and moves `current`.
const CODEX_APPS: &[&str] = &["codex", "codex-code-mode-host"];

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub struct CodexReleases;

impl CleanProvider for CodexReleases {
    fn id(&self) -> &'static str {
        ID
    }
    fn label(&self) -> &'static str {
        LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Codex releases its current link no longer points to; the release in use is never listed"
            .into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    fn quit_apps(&self) -> Vec<&'static str> {
        CODEX_APPS.to_vec()
    }
    fn desktop_trash_paths(&self) -> bool {
        true
    }
    fn available(&self) -> bool {
        home().is_some_and(|h| is_dir_safe(&h.join(PACKAGES_DIR)))
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        Ok(home()
            .map(|h| superseded_releases(ctx, &h.join(PACKAGES_DIR)))
            .unwrap_or_default())
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_per_item(items, action, ID)
    }
    fn check_item(&self, path: &Path) -> std::result::Result<(), String> {
        let packages = home().ok_or("HOME is not set")?.join(PACKAGES_DIR);
        check_superseded(&packages, path)
    }
}

/// Device and inode of a real directory; `None` for anything else.
fn dir_identity(path: &Path) -> Option<(u64, u64)> {
    let meta = fs::symlink_metadata(path).ok()?;
    meta.file_type().is_dir().then(|| (meta.dev(), meta.ino()))
}

/// The real directory directly inside `<package>/releases` that
/// `<package>/current` links to. `None` when the link is missing, not a
/// symlink, unreadable or dangling, or names anything else.
fn current_release(package: &Path) -> Option<(u64, u64)> {
    let link = package.join(CURRENT);
    if !fs::symlink_metadata(&link).ok()?.file_type().is_symlink() {
        return None;
    }
    let target = package.join(fs::read_link(&link).ok()?);
    if target.parent() != Some(package.join(RELEASES).as_path()) {
        return None;
    }
    dir_identity(&target)
}

/// Every release folder except the current one, per package. A package
/// whose `current` cannot be trusted offers nothing. Releases are compared
/// by identity, not name, so a link spelled in another case still protects
/// its release.
fn superseded_releases(ctx: &ScanContext<'_>, packages: &Path) -> Vec<CleanItem> {
    let mut items = Vec::new();
    for package in list_children(packages, ctx) {
        if !is_dir_safe(&package) {
            continue;
        }
        let Some(current) = current_release(&package) else {
            continue;
        };
        for path in list_children(&package.join(RELEASES), ctx) {
            if dir_identity(&path).is_some_and(|id| id != current) {
                items.extend(root_as_item(ctx, &path, ID, LABEL, RiskLevel::Review));
            }
        }
    }
    items
}

/// Re-checked right before acting: `path` is still a release folder of a
/// package in `packages`, and `current` still resolves to another one.
fn check_superseded(packages: &Path, path: &Path) -> std::result::Result<(), String> {
    let package = path
        .parent()
        .filter(|releases| releases.file_name() == Some(OsStr::new(RELEASES)))
        .and_then(Path::parent)
        .filter(|package| package.parent() == Some(packages));
    let (Some(package), Some(id)) = (package, dir_identity(path)) else {
        return Err("not a Codex release folder".into());
    };
    match current_release(package) {
        Some(current) if current != id => Ok(()),
        Some(_) => Err("it is the current Codex release".into()),
        None => Err("the current Codex release cannot be read".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::symlink;

    const NEW: &str = "0.163.0-aarch64-apple-darwin";
    const OLD: &str = "0.162.1-aarch64-apple-darwin";

    /// `<packages>/<name>` with the given releases, laid out as Codex does.
    fn package(packages: &Path, name: &str, releases: &[&str]) -> PathBuf {
        let package = packages.join(name);
        for release in releases {
            let bin = package.join(RELEASES).join(release).join("bin");
            fs::create_dir_all(&bin).unwrap();
            fs::write(bin.join("codex"), b"binary").unwrap();
        }
        fs::create_dir_all(&package).unwrap();
        fs::write(package.join("install.lock"), b"").unwrap();
        fs::write(package.join("auto-update-version"), NEW).unwrap();
        package
    }

    fn sorted_paths(items: &[CleanItem]) -> Vec<PathBuf> {
        let mut paths: Vec<_> = items.iter().map(|i| i.path.clone()).collect();
        paths.sort();
        paths
    }

    fn discover(packages: &Path) -> Vec<PathBuf> {
        sorted_paths(&superseded_releases(&ScanContext::unchecked(), packages))
    }

    #[test]
    fn offers_every_release_but_the_one_current_targets() {
        let home = tempfile::tempdir().unwrap();
        let codex = home.path().join(".codex");
        let packages = codex.join("packages");
        let standalone = package(&packages, "standalone", &[OLD, NEW]);
        symlink(
            standalone.join(RELEASES).join(NEW),
            standalone.join(CURRENT),
        )
        .unwrap();
        let daemon = package(&packages, "app-server-daemon", &["0.161.0", OLD]);
        symlink(Path::new(RELEASES).join(OLD), daemon.join(CURRENT)).unwrap();
        // Neither a stray file nor a link in releases/ is a release.
        fs::write(standalone.join(RELEASES).join("notes"), b"x").unwrap();
        symlink(
            standalone.join(RELEASES).join(OLD),
            standalone.join(RELEASES).join("latest"),
        )
        .unwrap();
        // Codex's own data beside the packages is never a candidate.
        for data in ["sessions/2026", "worktrees/a1b2", "sqlite"] {
            fs::create_dir_all(codex.join(data)).unwrap();
        }
        for file in ["auth.json", "history.jsonl", "state_5.sqlite"] {
            fs::write(codex.join(file), b"{}").unwrap();
        }
        let items = superseded_releases(&ScanContext::unchecked(), &packages);
        assert_eq!(
            sorted_paths(&items),
            vec![
                daemon.join(RELEASES).join("0.161.0"),
                standalone.join(RELEASES).join(OLD),
            ]
        );
        assert!(items.iter().all(|i| i.risk == RiskLevel::Review));
        assert!(items.iter().all(|i| i.size > 0));
    }

    #[test]
    fn offers_nothing_when_current_cannot_be_trusted() {
        let home = tempfile::tempdir().unwrap();
        let packages = home.path().join("packages");
        let elsewhere = home.path().join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        let cases: [(&str, Option<PathBuf>); 5] = [
            ("missing", None),
            ("dangling", Some(PathBuf::from("releases/0.999.0"))),
            ("outside", Some(elsewhere.clone())),
            (
                "escapes",
                Some(PathBuf::from("releases/../releases").join(NEW)),
            ),
            ("through-a-link", Some(PathBuf::from("releases/latest"))),
        ];
        for (name, target) in cases {
            let package = package(&packages, name, &[OLD, NEW]);
            symlink(NEW, package.join(RELEASES).join("latest")).unwrap();
            if let Some(target) = target {
                symlink(target, package.join(CURRENT)).unwrap();
            }
        }
        let not_a_link = package(&packages, "not-a-link", &[OLD, NEW]);
        fs::create_dir_all(not_a_link.join(CURRENT)).unwrap();
        assert_eq!(discover(&packages), Vec::<PathBuf>::new());
    }

    #[test]
    fn a_link_spelled_in_another_case_still_protects_its_release() {
        let home = tempfile::tempdir().unwrap();
        let packages = home.path().join("packages");
        let package = package(&packages, "standalone", &[OLD, NEW]);
        symlink(
            Path::new(RELEASES).join(NEW.to_uppercase()),
            package.join(CURRENT),
        )
        .unwrap();
        // Case-insensitive volume: NEW is current. Case-sensitive: the link
        // dangles and nothing is offered. Never NEW either way.
        assert!(!discover(&packages).contains(&package.join(RELEASES).join(NEW)));
    }

    #[test]
    fn check_item_refuses_the_current_release_even_after_a_rollback() {
        let home = tempfile::tempdir().unwrap();
        let codex = home.path().join(".codex");
        let packages = codex.join("packages");
        let package = package(&packages, "standalone", &[OLD, NEW]);
        let (old, new) = (
            package.join(RELEASES).join(OLD),
            package.join(RELEASES).join(NEW),
        );
        symlink(&new, package.join(CURRENT)).unwrap();
        assert_eq!(check_superseded(&packages, &old), Ok(()));
        assert_eq!(
            check_superseded(&packages, &new),
            Err("it is the current Codex release".into())
        );
        // Codex rolled back between the scan and the clean.
        fs::remove_file(package.join(CURRENT)).unwrap();
        symlink(&old, package.join(CURRENT)).unwrap();
        assert!(check_superseded(&packages, &old).is_err());
        assert!(check_superseded(&packages, &new).is_ok());
        // A dangling link, or one to a folder outside releases/, refuses everything.
        let elsewhere = home.path().join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        for target in [Path::new("releases/0.999.0"), &elsewhere] {
            fs::remove_file(package.join(CURRENT)).unwrap();
            symlink(target, package.join(CURRENT)).unwrap();
            for release in [&old, &new] {
                assert_eq!(
                    check_superseded(&packages, release),
                    Err("the current Codex release cannot be read".into())
                );
            }
        }
        fs::create_dir_all(codex.join("sessions")).unwrap();
        for outside in [
            codex.join("sessions"),
            package.join("install.lock"),
            package.join(RELEASES),
            package.clone(),
            new.join("bin"),
        ] {
            assert_eq!(
                check_superseded(&packages, &outside),
                Err("not a Codex release folder".into()),
                "{}",
                outside.display()
            );
        }
    }

    #[test]
    fn codex_releases_wait_for_every_codex_process() {
        assert_eq!(CodexReleases.quit_apps(), CODEX_APPS);
        assert_eq!(CodexReleases.item_apps(Path::new("/x")), CODEX_APPS);
    }
}
