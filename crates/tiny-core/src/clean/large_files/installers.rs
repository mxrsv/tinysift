//! Group 2, Installers: a `.dmg`, `.pkg`, `.iso` or `.xip` whose app is
//! already in Applications (IN1). The match is the file name against the
//! names of the app bundles found there, nothing else; an installer with no
//! such app is not this group's (later groups place it).

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::clean::fs_safe::PathFingerprint;
use crate::clean::private_size::private_allocated_bytes;

use super::app_version::AppVersions;
use super::name_match::{is_newer, parse_name, parse_version};
use super::{FreesNothing, Group, LargeFile};

/// 100 MB (GR3, CU12), judged on private bytes: a file whose blocks another
/// file shares frees less than it measures.
pub const INSTALLER_MIN_BYTES: u64 = 100_000_000;

/// What makes a file an installer.
pub const INSTALLER_EXTENSIONS: &[&str] = &["dmg", "pkg", "iso", "xip"];

/// Where to look, injectable so a test never reads the real home.
#[derive(Debug, Clone)]
pub struct InstallerRoots {
    /// Folders whose direct children are looked at (Downloads, Desktop).
    pub folders: Vec<PathBuf>,
    /// Folders whose direct `.app` children count as installed.
    pub applications: Vec<PathBuf>,
}

impl InstallerRoots {
    pub fn for_home(home: &Path) -> Self {
        Self {
            folders: vec![home.join("Downloads"), home.join("Desktop")],
            applications: vec![PathBuf::from("/Applications"), home.join("Applications")],
        }
    }
}

/// What the scan found, and the folders it could not read.
#[derive(Debug, Clone, Default)]
pub struct InstallerScan {
    /// By what each frees, largest first.
    pub files: Vec<LargeFile>,
    /// Folders that exist but could not be listed (a denied permission): an
    /// empty result then does not mean "nothing there".
    pub unreadable: Vec<PathBuf>,
    /// Installer-type files of the threshold's size whose blocks are all
    /// shared (FN1): any name, matched or not.
    pub frees_nothing: Vec<FreesNothing>,
}

struct InstalledApp {
    name: String,
    path: PathBuf,
    key: String,
}

/// Reads only: lists folders and file metadata, runs no tool except through
/// `versions`, and only for an installer whose name carries a version.
pub fn scan_installers(
    roots: &InstallerRoots,
    versions: &dyn AppVersions,
    cancel: Option<&AtomicBool>,
) -> InstallerScan {
    scan_installers_above(INSTALLER_MIN_BYTES, roots, versions, cancel)
}

/// `scan_installers` with the size threshold given, so tests need no 100 MB
/// fixtures.
pub fn scan_installers_above(
    min_bytes: u64,
    roots: &InstallerRoots,
    versions: &dyn AppVersions,
    cancel: Option<&AtomicBool>,
) -> InstallerScan {
    let apps = installed_apps(&roots.applications);
    let mut scan = InstallerScan::default();
    for folder in &roots.folders {
        let entries = match fs::read_dir(folder) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(_) => {
                scan.unreadable.push(folder.clone());
                continue;
            }
        };
        for entry in entries.flatten() {
            if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
                return scan;
            }
            if let Some(file) = installer(
                &entry.path(),
                min_bytes,
                &apps,
                versions,
                &mut scan.frees_nothing,
            ) {
                scan.files.push(file);
            }
        }
    }
    scan.files
        .sort_by(|a, b| b.frees.bytes.cmp(&a.frees.bytes).then(a.path.cmp(&b.path)));
    scan
}

fn installer(
    path: &Path,
    min_bytes: u64,
    apps: &[InstalledApp],
    versions: &dyn AppVersions,
    frees_nothing: &mut Vec<FreesNothing>,
) -> Option<LargeFile> {
    let extension = path.extension()?.to_str()?.to_lowercase();
    if !INSTALLER_EXTENSIONS.contains(&extension.as_str()) {
        return None;
    }
    // Symlinks and folders are not the downloaded file itself.
    let meta = fs::symlink_metadata(path).ok().filter(|m| m.is_file())?;
    if meta.blocks().saturating_mul(512) < min_bytes {
        return None;
    }
    // A file that frees nothing (all blocks shared, or an iCloud placeholder
    // with none on disk) cannot be picked: group 5 explains it (FN1).
    let frees = private_allocated_bytes(path).ok()?;
    if frees.bytes == 0 {
        frees_nothing.push(FreesNothing {
            path: path.to_path_buf(),
            allocated: meta.blocks().saturating_mul(512),
            modified: meta.modified().ok(),
            fingerprint: PathFingerprint::of(&meta),
        });
        return None;
    }
    let parsed = parse_name(path.file_stem()?.to_str()?);
    let app = apps
        .iter()
        .find(|app| !parsed.key.is_empty() && app.key == parsed.key)?;
    if frees.bytes < min_bytes {
        return None;
    }
    let newer = parsed
        .version
        .as_deref()
        .zip(
            versions
                .version(&app.path)
                .as_deref()
                .and_then(parse_version),
        )
        .is_some_and(|(installer, installed)| is_newer(&installed, installer));
    Some(LargeFile {
        path: path.to_path_buf(),
        name: format!("{} installer", app.name),
        group: Group::Installers,
        reason: reason(&app.name, newer),
        frees,
        matched_app: Some(app.path.clone()),
        fingerprint: PathFingerprint::of(&meta),
        elsewhere: None,
        copy: None,
    })
}

/// A fact each time: the app is in Applications; "newer" only when both
/// versions parsed and the installed one is higher (LR1).
fn reason(app: &str, newer: bool) -> String {
    match newer {
        true => format!("A newer {app} is installed in Applications"),
        false => format!("{app} is installed in Applications"),
    }
}

/// The `.app` bundles directly inside each root, the first root's winning a
/// name two roots share.
fn installed_apps(roots: &[PathBuf]) -> Vec<InstalledApp> {
    let mut apps: Vec<InstalledApp> = Vec::new();
    for root in roots {
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        let mut found: Vec<InstalledApp> = entries
            .flatten()
            .filter_map(|entry| {
                let path = entry.path();
                let stem = path
                    .file_name()?
                    .to_str()?
                    .strip_suffix(".app")?
                    .to_string();
                let is_bundle = fs::symlink_metadata(&path).ok()?.is_dir();
                let key = parse_name(&stem).key;
                (is_bundle && !key.is_empty()).then_some(InstalledApp {
                    name: stem,
                    path,
                    key,
                })
            })
            .collect();
        found.sort_by(|a, b| a.path.cmp(&b.path));
        for app in found {
            if !apps.iter().any(|known| known.key == app.key) {
                apps.push(app);
            }
        }
    }
    apps
}
