//! Storage queries exported to Swift: startup-disk capacity and a depth-limited
//! size tree. Results are observational; nothing here grants authority to move
//! or delete a path.

use std::path::Path;
use std::sync::Arc;

use tiny_core::space_lens::{self, SpaceNode, SpaceReport};
use tiny_core::sys::{self, DiskCapacity};

use crate::{CancellationToken, FfiError, ProgressListener, TinySession};

#[cfg(test)]
mod tests;

/// Capacity of the volume holding the user's data.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiDiskCapacity {
    pub name: String,
    pub mount_point: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
}

/// One file or folder. `bytes` covers everything readable below it, including
/// entries omitted from `children`.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiSpaceNode {
    pub name: String,
    pub path: String,
    pub bytes: u64,
    pub allocated_bytes: u64,
    pub age_seconds: u64,
    pub is_directory: bool,
    pub children: Vec<FfiSpaceNode>,
    /// Entries counted in `bytes` but not in `children`.
    pub omitted_count: u64,
    pub omitted_bytes: u64,
    /// Something below could not be read, so `bytes` is a lower bound.
    pub unreadable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct FfiStorageReport {
    pub root: FfiSpaceNode,
    pub files_scanned: u64,
    /// First unreadable paths; `skipped_count` is the full total.
    pub skipped_paths: Vec<String>,
    pub skipped_count: u64,
    /// Some folder leaves children out; totals stay complete.
    pub truncated: bool,
    /// Some size is a lower bound.
    pub incomplete: bool,
}

impl From<DiskCapacity> for FfiDiskCapacity {
    fn from(disk: DiskCapacity) -> Self {
        Self {
            name: disk.name,
            mount_point: disk.mount_point,
            total_bytes: disk.total_bytes,
            available_bytes: disk.available_bytes,
        }
    }
}

impl From<SpaceNode> for FfiSpaceNode {
    fn from(node: SpaceNode) -> Self {
        Self {
            name: node.name,
            path: node.path,
            bytes: node.bytes,
            allocated_bytes: node.allocated_bytes,
            age_seconds: node.age_seconds,
            is_directory: node.is_directory,
            children: node.children.into_iter().map(Self::from).collect(),
            omitted_count: node.omitted_count,
            omitted_bytes: node.omitted_bytes,
            unreadable: node.unreadable,
        }
    }
}

impl From<SpaceReport> for FfiStorageReport {
    fn from(report: SpaceReport) -> Self {
        Self {
            root: report.root.into(),
            files_scanned: report.files_scanned,
            skipped_paths: report.skipped_paths,
            skipped_count: report.skipped_count,
            truncated: report.truncated,
            incomplete: report.incomplete,
        }
    }
}

/// Startup-disk capacity, or `None` when no trustworthy sample exists. A quick
/// read that bypasses the operation gate, like process queries.
#[uniffi::export]
pub fn disk_capacity() -> Option<FfiDiskCapacity> {
    sys::startup_capacity(&sys::disks()).map(Into::into)
}

#[uniffi::export]
impl TinySession {
    /// Scans `root` (an absolute path without symlinked ancestors) into a
    /// depth-limited tree; drill down by scanning a child path. Holds the
    /// operation gate, so it returns `Busy` while another scan or mutation
    /// runs. A cancelled scan returns `Cancelled` and no partial tree.
    pub fn storage_scan(
        &self,
        root: String,
        token: Arc<CancellationToken>,
        progress: Arc<dyn ProgressListener>,
    ) -> Result<FfiStorageReport, FfiError> {
        self.scan_storage(Path::new(&root), &token, progress.as_ref())
    }
}

impl TinySession {
    /// `storage_scan` with the listener as a plain reference (the test seam).
    pub(crate) fn scan_storage(
        &self,
        root: &Path,
        token: &CancellationToken,
        progress: &dyn ProgressListener,
    ) -> Result<FfiStorageReport, FfiError> {
        let _gate = self.begin()?;
        let report = |p: tiny_core::progress::Progress| progress.on_progress(p.into());
        let scanned = space_lens::scan(root, Some(token.flag()), Some(&report))?;
        // A flag set after the last checkpoint still discards the tree.
        token.check()?;
        if scanned.cancelled {
            return Err(FfiError::Cancelled);
        }
        Ok(scanned.into())
    }
}
