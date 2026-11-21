//! Group 5 items an app owns, beyond UTM's and Cap's libraries (LV-L5,
//! LV-L6): virtual machines (Parallels, VMware Fusion, VirtualBox, UTM
//! bundles kept outside UTM's folder, Colima), sparse disk image bundles,
//! Docker Desktop's disk image, Finder's device backups and the local copies
//! a cloud app keeps in `~/Library/CloudStorage`. Each is listed with the
//! app that owns it and never moved from here ([`super::Group::movable`]).
//!
//! Two ways in. The home walk meets bundles and VirtualBox folders where the
//! user keeps them ([`owner_of_folder`], [`virtualbox_vm`]): each is
//! recognised by its format's own marker file, not by its name alone (LR1).
//! The places the walk skips (`~/Library`, dot-folders) are read only through
//! [`KNOWN_LOCATIONS`], each opened only to measure an item and name its
//! owner. The paths follow each app's documented default layout; none of
//! them existed on the Mac this was written on, so tests use fixtures.

use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use super::discovery::is_dataless;
use super::elsewhere::utm_name;
use super::lens::Lens;

/// One item of group 5 an app owns, with the words that name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OwnedItem {
    pub path: PathBuf,
    /// The row's name ("Windows 11 · Parallels virtual machine").
    pub name: String,
    /// "Virtual machine", "Disk image", "Device backup", "Cloud-synced copy".
    pub kind: &'static str,
    pub lens: Lens,
    pub owner: Owner,
    /// One sentence: where to act, then the checked fact (LR1).
    pub reason: String,
}

/// The app that owns an item, as far as the scan can name it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Owner {
    pub app_name: String,
    /// The `.app` to look for in the Applications folders, without `.app`.
    pub app_file: Option<String>,
    /// Where the app is when it ships with macOS (Finder, Disk Utility).
    pub app_path: Option<&'static str>,
    pub bundle_id: Option<String>,
    /// The process name `pgrep -x` matches, when it is known.
    pub process_name: Option<&'static str>,
}

/// A bundle format the walk recognises: its extension and the entry inside
/// that proves the format.
struct BundleFormat {
    extension: &'static str,
    marker: Marker,
    app: StaticOwner,
    kind: &'static str,
    lens: Lens,
    /// "virtual machine" in "Windows · Parallels virtual machine".
    noun: &'static str,
    act: &'static str,
}

#[derive(Clone, Copy)]
enum Marker {
    /// A file of this name directly inside.
    File(&'static str),
    /// A file and a folder of these names directly inside.
    FileAndFolder(&'static str, &'static str),
    /// Any file with this extension directly inside.
    Extension(&'static str),
}

#[derive(Clone, Copy)]
struct StaticOwner {
    app_name: &'static str,
    app_file: Option<&'static str>,
    app_path: Option<&'static str>,
    bundle_id: Option<&'static str>,
    process_name: Option<&'static str>,
}

impl StaticOwner {
    fn owner(self) -> Owner {
        Owner {
            app_name: self.app_name.to_string(),
            app_file: self.app_file.map(str::to_string),
            app_path: self.app_path,
            bundle_id: self.bundle_id.map(str::to_string),
            process_name: self.process_name,
        }
    }
}

const PARALLELS: StaticOwner = StaticOwner {
    app_name: "Parallels Desktop",
    app_file: Some("Parallels Desktop"),
    app_path: None,
    bundle_id: Some("com.parallels.desktop.console"),
    process_name: Some("prl_client_app"),
};
const VMWARE: StaticOwner = StaticOwner {
    app_name: "VMware Fusion",
    app_file: Some("VMware Fusion"),
    app_path: None,
    bundle_id: Some("com.vmware.fusion"),
    process_name: Some("VMware Fusion"),
};
const VIRTUALBOX: StaticOwner = StaticOwner {
    app_name: "VirtualBox",
    app_file: Some("VirtualBox"),
    app_path: None,
    bundle_id: Some("org.virtualbox.app.VirtualBox"),
    process_name: Some("VirtualBox"),
};
const UTM: StaticOwner = StaticOwner {
    app_name: "UTM",
    app_file: Some("UTM"),
    app_path: None,
    bundle_id: Some("com.utmapp.UTM"),
    process_name: Some("UTM"),
};
const DISK_UTILITY: StaticOwner = StaticOwner {
    app_name: "Disk Utility",
    app_file: None,
    app_path: Some("/System/Applications/Utilities/Disk Utility.app"),
    bundle_id: Some("com.apple.DiskUtility"),
    process_name: None,
};
const DOCKER: StaticOwner = StaticOwner {
    app_name: "Docker Desktop",
    app_file: Some("Docker"),
    app_path: None,
    bundle_id: Some("com.docker.docker"),
    process_name: None,
};
const COLIMA: StaticOwner = StaticOwner {
    app_name: "Colima",
    app_file: None,
    app_path: None,
    bundle_id: None,
    process_name: None,
};
const FINDER: StaticOwner = StaticOwner {
    app_name: "Finder",
    app_file: None,
    app_path: Some("/System/Library/CoreServices/Finder.app"),
    bundle_id: Some("com.apple.finder"),
    process_name: None,
};

const VM_ACT: &str = "Remove it in {app}, not here";

/// Bundles the walk lists as one group 5 item instead of entering them.
const BUNDLES: &[BundleFormat] = &[
    BundleFormat {
        extension: "pvm",
        marker: Marker::File("config.pvs"),
        app: PARALLELS,
        kind: "Virtual machine",
        lens: Lens::VirtualMachine,
        noun: "virtual machine",
        act: VM_ACT,
    },
    BundleFormat {
        extension: "vmwarevm",
        marker: Marker::Extension("vmx"),
        app: VMWARE,
        kind: "Virtual machine",
        lens: Lens::VirtualMachine,
        noun: "virtual machine",
        act: VM_ACT,
    },
    BundleFormat {
        extension: "utm",
        marker: Marker::File("config.plist"),
        app: UTM,
        kind: "Virtual machine",
        lens: Lens::VirtualMachine,
        noun: "virtual machine",
        act: VM_ACT,
    },
    BundleFormat {
        extension: "sparsebundle",
        marker: Marker::FileAndFolder("Info.plist", "bands"),
        app: DISK_UTILITY,
        kind: "Disk image",
        lens: Lens::Installer,
        noun: "disk image",
        act: "Open it in {app}, not here",
    },
];

/// The places the walk does not enter that group 5 reads (LV-L5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KnownLocation {
    /// `Docker.raw` (or the older `Docker.qcow2`) of Docker Desktop.
    DockerDisk,
    /// Each Colima instance and data disk under `~/.colima/_lima`.
    Colima,
    /// Each device backup Finder keeps.
    DeviceBackups,
    /// Every local file of 100 MB or more a cloud app keeps.
    CloudStorage,
}

/// Home-relative folder of each known location.
pub const KNOWN_LOCATIONS: &[(KnownLocation, &str)] = &[
    (
        KnownLocation::DockerDisk,
        "Library/Containers/com.docker.docker/Data/vms/0/data",
    ),
    (KnownLocation::Colima, ".colima/_lima"),
    (
        KnownLocation::DeviceBackups,
        "Library/Application Support/MobileSync/Backup",
    ),
    (KnownLocation::CloudStorage, "Library/CloudStorage"),
];

const DOCKER_DISKS: &[&str] = &["Docker.raw", "Docker.qcow2"];
/// Lima's disk files: an instance's own disk, and a Colima data disk.
const COLIMA_DISKS: &[&str] = &["diffdisk", "basedisk", "datadisk"];

/// Cloud apps by the first word of their `CloudStorage` folder
/// (`GoogleDrive-name@example.com`, `OneDrive-Personal`, `Dropbox`).
const CLOUD_APPS: &[(&str, StaticOwner)] = &[
    (
        "Dropbox",
        StaticOwner {
            app_name: "Dropbox",
            app_file: Some("Dropbox"),
            app_path: None,
            bundle_id: Some("com.getdropbox.dropbox"),
            process_name: Some("Dropbox"),
        },
    ),
    (
        "GoogleDrive",
        StaticOwner {
            app_name: "Google Drive",
            app_file: Some("Google Drive"),
            app_path: None,
            bundle_id: Some("com.google.drivefs"),
            process_name: Some("Google Drive"),
        },
    ),
    (
        "OneDrive",
        StaticOwner {
            app_name: "OneDrive",
            app_file: Some("OneDrive"),
            app_path: None,
            bundle_id: Some("com.microsoft.OneDrive"),
            process_name: Some("OneDrive"),
        },
    ),
    (
        "Box",
        StaticOwner {
            app_name: "Box",
            app_file: Some("Box"),
            app_path: None,
            bundle_id: Some("com.box.desktop"),
            process_name: Some("Box"),
        },
    ),
];

/// The item a folder the walk met is, when its format's marker is inside.
/// A folder with a known extension but no marker is not listed; the walk
/// still does not enter it.
pub fn owner_of_folder(path: &Path) -> Option<OwnedItem> {
    let extension = path.extension()?.to_str()?.to_lowercase();
    let format = BUNDLES.iter().find(|f| f.extension == extension)?;
    if !has_marker(path, format.marker) {
        return None;
    }
    let stem = path.file_stem()?.to_string_lossy().into_owned();
    let label = match format.extension {
        "utm" => utm_name(path).unwrap_or(stem),
        _ => stem,
    };
    let app = format.app.app_name;
    Some(OwnedItem {
        path: path.to_path_buf(),
        name: format!("{label} · {app} {}", format.noun),
        kind: format.kind,
        lens: format.lens,
        owner: format.app.owner(),
        reason: format!(
            "{} · a {app} {}",
            format.act.replace("{app}", app),
            format.noun
        ),
    })
}

/// A VirtualBox machine: a folder holding its `.vbox` settings file, listed
/// as one item (the walk then skips what is inside). `names` are the
/// folder's entries.
pub fn virtualbox_vm(dir: &Path, names: &[(String, bool)]) -> Option<OwnedItem> {
    names
        .iter()
        .any(|(name, is_file)| *is_file && has_extension(name, "vbox"))
        .then(|| {
            let label = dir
                .file_name()
                .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
            vm_item(dir, &label, VIRTUALBOX)
        })
}

fn vm_item(path: &Path, label: &str, app: StaticOwner) -> OwnedItem {
    OwnedItem {
        path: path.to_path_buf(),
        name: format!("{label} · {} virtual machine", app.app_name),
        kind: "Virtual machine",
        lens: Lens::VirtualMachine,
        owner: app.owner(),
        reason: format!(
            "{} · a {} virtual machine",
            VM_ACT.replace("{app}", app.app_name),
            app.app_name
        ),
    }
}

/// What the known locations hold, and those that exist but could not be
/// listed (a missing Full Disk Access for device backups).
#[derive(Debug, Default)]
pub struct KnownScan {
    pub items: Vec<OwnedItem>,
    pub unreadable: Vec<PathBuf>,
}

/// Reads every known location of `home` on the volume `device`. Cloud files
/// need `min_bytes` of allocated blocks; the other items are whole. iCloud
/// and File Provider placeholders, hidden names and git work trees are
/// never listed. A cancelled scan returns what it had.
pub fn scan_known_locations(
    home: &Path,
    device: u64,
    min_bytes: u64,
    cancel: Option<&AtomicBool>,
) -> KnownScan {
    let mut scan = KnownScan::default();
    for (location, folder) in KNOWN_LOCATIONS {
        if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            break;
        }
        let root = home.join(folder);
        let Some(children) = list(&root, device, &mut scan.unreadable) else {
            continue;
        };
        match location {
            KnownLocation::DockerDisk => {
                for (path, meta) in children {
                    let named = path
                        .file_name()
                        .and_then(|n| n.to_str())
                        .is_some_and(|n| DOCKER_DISKS.contains(&n));
                    if named && meta.is_file() {
                        scan.items.push(docker_item(&path));
                    }
                }
            }
            KnownLocation::Colima => colima_items(&children, device, &mut scan),
            KnownLocation::DeviceBackups => {
                for (path, meta) in children {
                    if meta.is_dir() {
                        scan.items.push(backup_item(&path));
                    }
                }
            }
            KnownLocation::CloudStorage => {
                for (path, meta) in children {
                    if meta.is_dir() {
                        cloud_items(&path, device, min_bytes, cancel, &mut scan);
                    }
                }
            }
        }
    }
    scan
}

/// The visible children of `dir` on `device` with their metadata; `None`
/// when it does not exist, and an unreadable folder is recorded.
fn list(
    dir: &Path,
    device: u64,
    unreadable: &mut Vec<PathBuf>,
) -> Option<Vec<(PathBuf, fs::Metadata)>> {
    let meta = fs::symlink_metadata(dir).ok()?;
    if !meta.is_dir() || meta.dev() != device || is_dataless(&meta) {
        return None;
    }
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(error) => {
            if error.kind() != std::io::ErrorKind::NotFound {
                unreadable.push(dir.to_path_buf());
            }
            return None;
        }
    };
    let mut children: Vec<(PathBuf, fs::Metadata)> = entries
        .flatten()
        .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
        .filter_map(|e| {
            let meta = fs::symlink_metadata(e.path()).ok()?;
            (meta.dev() == device && !is_dataless(&meta)).then(|| (e.path(), meta))
        })
        .collect();
    children.sort_by(|a, b| a.0.cmp(&b.0));
    Some(children)
}

fn docker_item(path: &Path) -> OwnedItem {
    OwnedItem {
        path: path.to_path_buf(),
        name: "Docker Desktop disk image".to_string(),
        kind: "Virtual machine disk",
        lens: Lens::VirtualMachine,
        owner: DOCKER.owner(),
        reason: "Shrink it in Docker Desktop, not here · the disk of Docker's virtual machine"
            .to_string(),
    }
}

/// Instances (`_lima/<name>`) and data disks (`_lima/_disks/<name>`) that
/// hold one of Lima's disk files.
fn colima_items(children: &[(PathBuf, fs::Metadata)], device: u64, scan: &mut KnownScan) {
    for (path, meta) in children {
        if !meta.is_dir() {
            continue;
        }
        let name = path
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
        if name == "_disks" {
            let mut ignored = Vec::new();
            for (disk, disk_meta) in list(path, device, &mut ignored).unwrap_or_default() {
                if disk_meta.is_dir() && holds_any(&disk, COLIMA_DISKS) {
                    let label = disk
                        .file_name()
                        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
                    scan.items
                        .push(colima_item(&disk, &format!("{label} · Colima data disk")));
                }
            }
        } else if !name.starts_with('_') && holds_any(path, COLIMA_DISKS) {
            scan.items.push(colima_item(
                path,
                &format!("{name} · Colima virtual machine"),
            ));
        }
    }
}

fn colima_item(path: &Path, name: &str) -> OwnedItem {
    OwnedItem {
        path: path.to_path_buf(),
        name: name.to_string(),
        kind: "Virtual machine",
        lens: Lens::VirtualMachine,
        owner: COLIMA.owner(),
        reason: "Remove it with colima delete, not here · a Colima virtual machine".to_string(),
    }
}

/// A Finder device backup, named by its `Info.plist` `Device Name` when the
/// plist is XML.
fn backup_item(path: &Path) -> OwnedItem {
    let device = fs::read_to_string(path.join("Info.plist"))
        .ok()
        .and_then(|text| plist_string(&text, "Device Name"));
    let name = match &device {
        Some(device) => format!("{device} · device backup"),
        None => "Device backup".to_string(),
    };
    OwnedItem {
        path: path.to_path_buf(),
        name,
        kind: "Device backup",
        lens: Lens::Other,
        owner: FINDER.owner(),
        reason: "Remove it in Finder, not here · a device backup Finder keeps".to_string(),
    }
}

/// Every local file of `min_bytes` or more in one cloud app's folder.
fn cloud_items(
    folder: &Path,
    device: u64,
    min_bytes: u64,
    cancel: Option<&AtomicBool>,
    scan: &mut KnownScan,
) {
    let word = folder
        .file_name()
        .and_then(|n| n.to_str())
        .map(|n| n.split('-').next().unwrap_or(n).to_string())
        .unwrap_or_default();
    let owner = CLOUD_APPS
        .iter()
        .find(|(prefix, _)| *prefix == word)
        .map_or_else(
            || Owner {
                app_name: word.clone(),
                app_file: None,
                app_path: None,
                bundle_id: None,
                process_name: None,
            },
            |(_, app)| app.owner(),
        );
    let reason = format!(
        "Make it online-only in {app}, not here · it sits in {app}'s synced folder",
        app = owner.app_name
    );
    let mut pending = vec![folder.to_path_buf()];
    while let Some(dir) = pending.pop() {
        if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
            return;
        }
        // A git work tree is the developer's (NL1).
        if dir != folder && fs::symlink_metadata(dir.join(".git")).is_ok() {
            continue;
        }
        let Some(children) = list(&dir, device, &mut scan.unreadable) else {
            continue;
        };
        for (path, meta) in children {
            if meta.is_dir() {
                pending.push(path);
            } else if meta.is_file() && meta.blocks().saturating_mul(512) >= min_bytes {
                let name = path
                    .file_stem()
                    .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
                scan.items.push(OwnedItem {
                    lens: Lens::of_path(&path),
                    path,
                    name,
                    kind: "Cloud-synced copy",
                    owner: owner.clone(),
                    reason: reason.clone(),
                });
            }
        }
    }
}

fn has_marker(dir: &Path, marker: Marker) -> bool {
    let is_file = |name: &str| fs::symlink_metadata(dir.join(name)).is_ok_and(|m| m.is_file());
    match marker {
        Marker::File(name) => is_file(name),
        Marker::FileAndFolder(file, folder) => {
            is_file(file) && fs::symlink_metadata(dir.join(folder)).is_ok_and(|m| m.is_dir())
        }
        Marker::Extension(extension) => fs::read_dir(dir).is_ok_and(|entries| {
            entries.flatten().any(|e| {
                has_extension(&e.file_name().to_string_lossy(), extension)
                    && e.file_type().is_ok_and(|t| t.is_file())
            })
        }),
    }
}

fn holds_any(dir: &Path, names: &[&str]) -> bool {
    names
        .iter()
        .any(|name| fs::symlink_metadata(dir.join(name)).is_ok_and(|m| m.is_file()))
}

fn has_extension(name: &str, extension: &str) -> bool {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case(extension))
}

/// The `<string>` after `<key>key</key>` in an XML plist.
fn plist_string(text: &str, key: &str) -> Option<String> {
    let tag = format!("<key>{key}</key>");
    let after = &text[text.find(&tag)? + tag.len()..];
    let start = after.find("<string>")? + "<string>".len();
    // The value belongs to this key only when nothing else sits between.
    if !after[..start - "<string>".len()].trim().is_empty() {
        return None;
    }
    let end = after[start..].find("</string>")? + start;
    let value = after[start..end]
        .trim()
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&");
    (!value.is_empty()).then_some(value)
}
