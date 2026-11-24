//! Why the startup disk is full, exported to Swift: the container's used space split into System Settings' groups, what
//! could not be measured and why. Observational only; nothing here grants authority to move or delete a path.

use std::sync::Arc;

use tiny_core::disk_breakdown::{
    self, DiskBreakdown, DiskGroup, DiskItem, GroupReport, ItemKind, Refusal, RefusedPaths,
};

use crate::session::ScanSlot;
use crate::{CancellationToken, FfiError, ProgressListener, TinySession};

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiDiskGroup {
    Applications,
    Documents,
    Developer,
    ICloudDrive,
    OtherUsers,
    MacOs,
    SystemData,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiDiskItemKind {
    Folder,
    File,
    /// An app bundle; the name has no `.app`.
    App,
    /// An app container; the name is its folder's name (a bundle identifier, a team-prefixed group, or a UUID).
    Container,
    /// A volume, read from `statfs`; no path.
    Volume,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiDiskRefusal {
    /// macOS's privacy protection: Full Disk Access opens it.
    Privacy,
    OtherAccount,
    /// Only the system can read it.
    System,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiDiskItem {
    pub name: String,
    pub path: Option<String>,
    pub bytes: u64,
    pub kind: FfiDiskItemKind,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiDiskGroupReport {
    pub group: FfiDiskGroup,
    pub bytes: u64,
    /// Largest first; the rest are `smaller_count` items of `smaller_bytes`.
    pub items: Vec<FfiDiskItem>,
    pub smaller_count: u64,
    pub smaller_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiDiskRefusedPaths {
    pub refusal: FfiDiskRefusal,
    pub count: u64,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiDiskBreakdown {
    pub container_bytes: u64,
    pub available_bytes: u64,
    /// What the groups and `not_measured_bytes` add up to.
    pub used_bytes: u64,
    pub measured_bytes: u64,
    pub not_measured_bytes: u64,
    /// The walk found more than the volume reports: the gap is clamped to zero and `measured_bytes` is a floor.
    pub inexact: bool,
    pub entries_walked: u64,
    /// Every group, in the order clones are credited, empty ones included.
    pub groups: Vec<FfiDiskGroupReport>,
    pub refused: Vec<FfiDiskRefusedPaths>,
}

impl From<DiskGroup> for FfiDiskGroup {
    fn from(group: DiskGroup) -> Self {
        match group {
            DiskGroup::Applications => Self::Applications,
            DiskGroup::Documents => Self::Documents,
            DiskGroup::Developer => Self::Developer,
            DiskGroup::ICloudDrive => Self::ICloudDrive,
            DiskGroup::OtherUsers => Self::OtherUsers,
            DiskGroup::MacOs => Self::MacOs,
            DiskGroup::SystemData => Self::SystemData,
        }
    }
}

impl From<ItemKind> for FfiDiskItemKind {
    fn from(kind: ItemKind) -> Self {
        match kind {
            ItemKind::Folder => Self::Folder,
            ItemKind::File => Self::File,
            ItemKind::App => Self::App,
            ItemKind::Container => Self::Container,
            ItemKind::Volume => Self::Volume,
        }
    }
}

impl From<Refusal> for FfiDiskRefusal {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::Privacy => Self::Privacy,
            Refusal::OtherAccount => Self::OtherAccount,
            Refusal::System => Self::System,
        }
    }
}

impl From<DiskItem> for FfiDiskItem {
    fn from(item: DiskItem) -> Self {
        Self {
            name: item.name,
            path: item.path,
            bytes: item.bytes,
            kind: item.kind.into(),
        }
    }
}

impl From<GroupReport> for FfiDiskGroupReport {
    fn from(group: GroupReport) -> Self {
        Self {
            group: group.group.into(),
            bytes: group.bytes,
            items: group.items.into_iter().map(Into::into).collect(),
            smaller_count: group.smaller_count,
            smaller_bytes: group.smaller_bytes,
        }
    }
}

impl From<RefusedPaths> for FfiDiskRefusedPaths {
    fn from(refused: RefusedPaths) -> Self {
        Self {
            refusal: refused.refusal.into(),
            count: refused.count,
            paths: refused.paths,
        }
    }
}

impl From<DiskBreakdown> for FfiDiskBreakdown {
    fn from(report: DiskBreakdown) -> Self {
        Self {
            container_bytes: report.container_bytes,
            available_bytes: report.available_bytes,
            used_bytes: report.used_bytes,
            measured_bytes: report.measured_bytes,
            not_measured_bytes: report.not_measured_bytes,
            inexact: report.inexact,
            entries_walked: report.entries_walked,
            groups: report.groups.into_iter().map(Into::into).collect(),
            refused: report.refused.into_iter().map(Into::into).collect(),
        }
    }
}

#[uniffi::export]
impl TinySession {
    /// Measures the startup disk: the volume figures, then a read-only walk of the Data volume in private bytes. Holds
    /// Storage Explorer's scan slot of the operation gate, so it returns `Busy` while a write or a Storage scan runs. A
    /// cancelled walk returns `Cancelled` and nothing partial. Takes minutes on a full disk; progress counts entries
    /// against the Data volume's file count.
    pub fn disk_breakdown(
        &self,
        token: Arc<CancellationToken>,
        progress: Arc<dyn ProgressListener>,
    ) -> Result<FfiDiskBreakdown, FfiError> {
        self.measure_disk(&token, progress.as_ref())
    }
}

impl TinySession {
    /// `disk_breakdown` with the listener as a plain reference (the test seam).
    pub(crate) fn measure_disk(
        &self,
        token: &CancellationToken,
        progress: &dyn ProgressListener,
    ) -> Result<FfiDiskBreakdown, FfiError> {
        let _gate = self.begin_scan(ScanSlot::Storage)?;
        let report = |p: tiny_core::progress::Progress| progress.on_progress(p.into());
        let measured = disk_breakdown::measure(Some(token.flag()), Some(&report))?;
        token.check()?;
        if measured.cancelled {
            return Err(FfiError::Cancelled);
        }
        Ok(measured.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Silent;

    impl ProgressListener for Silent {
        fn on_progress(&self, _: crate::FfiProgress) {}
    }

    #[test]
    fn a_storage_scan_holds_the_measurement_off() {
        let session = TinySession::new();
        let _held = session
            .begin_scan(ScanSlot::Storage)
            .expect("the slot starts");
        assert!(matches!(
            session.measure_disk(&CancellationToken::default(), &Silent),
            Err(FfiError::Busy)
        ));
    }

    #[test]
    fn a_cancelled_token_returns_nothing() {
        let session = TinySession::new();
        let token = CancellationToken::default();
        token.cancel();
        assert!(matches!(
            session.measure_disk(&token, &Silent),
            Err(FfiError::Cancelled)
        ));
    }

    #[test]
    fn records_keep_every_figure() {
        let report = DiskBreakdown {
            container_bytes: 100,
            available_bytes: 30,
            used_bytes: 70,
            measured_bytes: 60,
            not_measured_bytes: 10,
            inexact: false,
            entries_walked: 5,
            groups: vec![GroupReport {
                group: DiskGroup::SystemData,
                bytes: 60,
                items: vec![DiskItem {
                    name: "com.utmapp.UTM".into(),
                    path: Some("/x".into()),
                    bytes: 50,
                    kind: ItemKind::Container,
                }],
                smaller_count: 1,
                smaller_bytes: 10,
            }],
            refused: vec![RefusedPaths {
                refusal: Refusal::Privacy,
                count: 3,
                paths: vec!["~/Library/Mail".into()],
            }],
            cancelled: false,
        };
        let ffi = FfiDiskBreakdown::from(report);
        assert_eq!(
            (ffi.used_bytes, ffi.measured_bytes, ffi.not_measured_bytes),
            (70, 60, 10)
        );
        assert_eq!(ffi.groups[0].group, FfiDiskGroup::SystemData);
        assert_eq!(ffi.groups[0].items[0].kind, FfiDiskItemKind::Container);
        assert_eq!(ffi.refused[0].refusal, FfiDiskRefusal::Privacy);
        assert_eq!(ffi.refused[0].paths, vec!["~/Library/Mail".to_string()]);
    }
}
