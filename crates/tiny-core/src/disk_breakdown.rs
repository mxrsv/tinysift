//! Why the startup disk is full: the container's used space split into the groups of System Settings › Storage, with
//! what could not be measured as its own figure and the reasons for it.
//!
//! The volumes the walk does not enter (System, Preboot, VM, Recovery…) come from `statfs`; the Data volume is walked
//! read-only and counted in private bytes (see [`walk`]). What the container uses that the volume figures and the walk
//! do not account for is `not_measured_bytes`, never dropped.

mod places;
mod volumes;
mod walk;

use crate::error::{Error, Result};
use crate::progress::{report, ProgressCallback};
use places::{Layout, Place};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;
use volumes::Container;

/// Most items a group keeps; the rest are summed in `smaller_*`. Generous, so the app can fold an app's containers
/// together before it picks the ten it shows.
const MAX_ITEMS: usize = 2_000;
/// Most paths kept per refusal reason; the count covers all.
const MAX_REFUSED_PATHS: usize = 8;
const WALK_THREADS: usize = 4;
const WALK_STACK_BYTES: usize = 16 * 1024 * 1024;

/// The groups of System Settings › Storage, in the order a clone or hard link shared between two of them is credited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DiskGroup {
    Applications,
    Documents,
    Developer,
    ICloudDrive,
    OtherUsers,
    MacOs,
    SystemData,
}

impl DiskGroup {
    pub const ALL: [DiskGroup; 7] = [
        Self::Applications,
        Self::Documents,
        Self::Developer,
        Self::ICloudDrive,
        Self::OtherUsers,
        Self::MacOs,
        Self::SystemData,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ItemKind {
    Folder,
    File,
    /// An app bundle; `name` is its name without `.app`.
    App,
    /// An app container (`~/Library/Containers/<id>`, `~/Library/Group Containers/<id>`); `name` is the folder's name.
    Container,
    /// A volume of the container, read from `statfs`; it has no path to open.
    Volume,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskItem {
    pub name: String,
    /// Where it is, to reveal or open; `None` for a volume.
    pub path: Option<String>,
    pub bytes: u64,
    pub kind: ItemKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupReport {
    pub group: DiskGroup,
    pub bytes: u64,
    /// Largest first, at most `MAX_ITEMS`.
    pub items: Vec<DiskItem>,
    pub smaller_count: u64,
    pub smaller_bytes: u64,
}

/// Why a folder could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum Refusal {
    /// macOS's privacy protection (`EPERM`): Full Disk Access opens it.
    Privacy,
    /// Another account's home folder.
    OtherAccount,
    /// Only the system can read it (`EACCES` and anything else).
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefusedPaths {
    pub refusal: Refusal,
    /// Folders refused, counted once per folder the walk tried.
    pub count: u64,
    /// The first paths shown (`~/Library/Containers`, `/Users/dev`), most refused first.
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskBreakdown {
    pub container_bytes: u64,
    pub available_bytes: u64,
    /// `container_bytes - available_bytes`: what the groups and the gap add up to.
    pub used_bytes: u64,
    pub measured_bytes: u64,
    pub not_measured_bytes: u64,
    /// The walk found more than the volume reports (files grew while it ran): the gap was clamped to zero and
    /// `measured_bytes` is at least the figure shown.
    pub inexact: bool,
    pub entries_walked: u64,
    /// Every group, in [`DiskGroup::ALL`] order, including empty ones.
    pub groups: Vec<GroupReport>,
    /// Only the reasons that occurred.
    pub refused: Vec<RefusedPaths>,
    /// Stopped early: nothing here may be shown.
    pub cancelled: bool,
}

/// Measures the startup disk. Read-only; stops early once `cancel` is set.
pub fn measure(
    cancel: Option<&AtomicBool>,
    progress: ProgressCallback<'_>,
) -> Result<DiskBreakdown> {
    let container = volumes::startup_container()?;
    let home = std::env::var_os("HOME")
        .ok_or_else(|| Error::Operation("The home folder is not known.".into()))?;
    measure_with(
        Path::new(volumes::DATA_MOUNT),
        Path::new(&home),
        &container,
        cancel,
        progress,
    )
}

/// `measure` of the Data volume mounted at `data_root`, with `home` as it appears from `/` (`/Users/me`).
pub(crate) fn measure_with(
    data_root: &Path,
    home: &Path,
    container: &Container,
    cancel: Option<&AtomicBool>,
    progress: ProgressCallback<'_>,
) -> Result<DiskBreakdown> {
    use std::os::unix::fs::MetadataExt;
    let layout = Layout {
        home: home
            .iter()
            .skip(1)
            .map(|part| part.to_string_lossy().into_owned())
            .collect(),
    };
    let device = std::fs::symlink_metadata(data_root)?.dev();
    let entries = AtomicU64::new(0);
    let claims = Mutex::new(HashMap::new());
    let total = Some(container.data_files).filter(|files| *files > 0);
    report(progress, "disk-breakdown", 0, total, "Measuring the disk");
    let walk = walk::Walk {
        layout: &layout,
        root: data_root,
        cancel,
        progress,
        total,
        entries: &entries,
        claims: &claims,
        device,
    };
    let pool = rayon::ThreadPoolBuilder::new()
        .num_threads(WALK_THREADS)
        .stack_size(WALK_STACK_BYTES)
        .thread_name(|index| format!("disk-breakdown-{index}"))
        .build()
        .map_err(|err| Error::Operation(format!("Could not start the walk: {err}")))?;
    let mut found = pool.install(|| {
        let mut branch = walk::Branch::new(walk);
        branch.walk_mixed(data_root, 0);
        branch.finish()
    });
    let cancelled = cancel.is_some_and(|flag| flag.load(Ordering::Acquire));
    for (_, (bytes, place)) in claims
        .into_inner()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
    {
        *found.items.entry(place).or_insert(0) += bytes;
    }
    Ok(assemble(
        data_root,
        container,
        found,
        entries.into_inner(),
        cancelled,
    ))
}

fn assemble(
    data_root: &Path,
    container: &Container,
    found: walk::Walked,
    entries_walked: u64,
    cancelled: bool,
) -> DiskBreakdown {
    let mut by_group: BTreeMap<DiskGroup, Vec<DiskItem>> = BTreeMap::new();
    for (place, bytes) in found.items.into_iter().filter(|(_, bytes)| *bytes > 0) {
        by_group
            .entry(place.group)
            .or_default()
            .push(item(data_root, place, bytes));
    }
    let used = container.used();
    let mounted: u64 = container
        .others
        .iter()
        .map(|volume| volume.used)
        .sum::<u64>()
        + container.data.used;
    for volume in &container.others {
        let group = if volume.mount == volumes::VM_MOUNT {
            DiskGroup::SystemData
        } else {
            DiskGroup::MacOs
        };
        let name = if group == DiskGroup::SystemData {
            "Swap (VM volume)".into()
        } else {
            volumes::volume_name(&volume.mount)
        };
        by_group.entry(group).or_default().push(DiskItem {
            name,
            path: None,
            bytes: volume.used,
            kind: ItemKind::Volume,
        });
    }
    let rest = used.saturating_sub(mounted);
    if rest > 0 {
        // Recovery is normally not mounted, so it is most of what the mounted volumes leave; when it is mounted it has its own row.
        let recovery = container
            .others
            .iter()
            .any(|volume| volumes::volume_name(&volume.mount) == "Recovery");
        by_group
            .entry(DiskGroup::MacOs)
            .or_default()
            .push(DiskItem {
                name: if recovery {
                    "Other volumes"
                } else {
                    "Recovery and other volumes"
                }
                .into(),
                path: None,
                bytes: rest,
                kind: ItemKind::Volume,
            });
    }
    let groups: Vec<GroupReport> = DiskGroup::ALL
        .iter()
        .map(|group| group_report(*group, by_group.remove(group).unwrap_or_default()))
        .collect();
    let measured: u64 = groups.iter().map(|group| group.bytes).sum();
    DiskBreakdown {
        container_bytes: container.total,
        available_bytes: container.available,
        used_bytes: used,
        measured_bytes: measured.min(used),
        not_measured_bytes: used.saturating_sub(measured),
        inexact: measured > used,
        entries_walked,
        groups,
        refused: refused(found.refused),
        cancelled,
    }
}

fn group_report(group: DiskGroup, mut items: Vec<DiskItem>) -> GroupReport {
    items.sort_by(|a, b| b.bytes.cmp(&a.bytes).then_with(|| a.name.cmp(&b.name)));
    let bytes = items.iter().map(|item| item.bytes).sum();
    let smaller: Vec<DiskItem> = items.split_off(items.len().min(MAX_ITEMS));
    GroupReport {
        group,
        bytes,
        items,
        smaller_count: smaller.len() as u64,
        smaller_bytes: smaller.iter().map(|item| item.bytes).sum(),
    }
}

/// The item's path as the user knows it: through the firmlink at `/` (`/Users/me/Downloads`) when that reaches the same
/// folder, else below the Data volume's mount.
fn item(data_root: &Path, place: Place, bytes: u64) -> DiskItem {
    use std::os::unix::fs::MetadataExt;
    let below = data_root.join(place.rel.join("/"));
    let firm = Path::new("/").join(place.rel.join("/"));
    let same =
        |a: &Path, b: &Path| match (std::fs::symlink_metadata(a), std::fs::symlink_metadata(b)) {
            (Ok(a), Ok(b)) => a.dev() == b.dev() && a.ino() == b.ino(),
            _ => false,
        };
    let path = if data_root != Path::new("/") && same(&firm, &below) {
        firm
    } else {
        below
    };
    DiskItem {
        name: place.name,
        path: Some(path.display().to_string()),
        bytes,
        kind: place.kind,
    }
}

fn refused(found: BTreeMap<(Refusal, String), u64>) -> Vec<RefusedPaths> {
    let mut by_reason: BTreeMap<Refusal, Vec<(String, u64)>> = BTreeMap::new();
    for ((refusal, path), count) in found {
        by_reason.entry(refusal).or_default().push((path, count));
    }
    by_reason
        .into_iter()
        .map(|(refusal, mut paths)| {
            paths.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            RefusedPaths {
                refusal,
                count: paths.iter().map(|(_, count)| count).sum(),
                paths: paths
                    .into_iter()
                    .take(MAX_REFUSED_PATHS)
                    .map(|(path, _)| path)
                    .collect(),
            }
        })
        .collect()
}

#[cfg(test)]
mod tests;
