//! Freed bytes measured around each removal: the item's freed bytes by the
//! [`freed_size`](crate::clean::freed_size) rule right before it, minus what
//! is still there right after. Discovery sizes never stand in for them.

use std::fs;
use std::io;
use std::path::Path;

use crate::clean::fs_safe::dir_size_checked;
use crate::clean::providers::{CleanProvider, DesktopTool};
use crate::clean::scan_context::ScanContext;
use crate::clean::trash_plan::{PlannedItem, Removal};

use super::report::{ItemOutcome, RemovalExtent};

#[cfg(test)]
type Hook = Option<Box<dyn FnMut(&Path)>>;

#[cfg(test)]
std::thread_local! {
    /// Runs right after an item's measurement before its removal, so tests
    /// can change the disk while the walk would take time.
    pub(super) static AFTER_MEASURE: std::cell::RefCell<Hook> = std::cell::RefCell::new(None);
}

/// One measurement of an item's freed bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(super) struct Measured {
    pub bytes: u64,
    pub upper_bound: bool,
}

/// What an item's removal freed and left, measured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Settled {
    pub extent: RemovalExtent,
    pub freed: u64,
    pub left: u64,
    pub upper_bound: bool,
}

/// Removals that free space when they succeed, so they are measured.
pub(super) fn frees_space(removal: Removal) -> bool {
    matches!(
        removal,
        Removal::Delete
            | Removal::DeleteAsAdministrator
            | Removal::EmptyTrash
            | Removal::RunTool(DesktopTool::Prune | DesktopTool::Remove { .. })
    )
}

/// `planned`'s freed bytes now: what its tool says it would still free
/// (`CleanProvider::tool_freeable`) for a tool removal that answers, the
/// item's path otherwise. A tool that cannot tell makes it 0 and a ceiling.
pub(super) fn measure_planned(
    planned: &PlannedItem,
    providers: &[Box<dyn CleanProvider>],
    unreadable_is_upper_bound: bool,
) -> Measured {
    if let Removal::RunTool(DesktopTool::Remove { .. }) = planned.removal {
        let answer = providers
            .iter()
            .find(|p| p.id() == planned.item.category_id)
            .and_then(|provider| provider.tool_freeable(&planned.item));
        match answer {
            Some(Ok(bytes)) => {
                return Measured {
                    bytes,
                    upper_bound: false,
                }
            }
            Some(Err(_)) => {
                return Measured {
                    bytes: 0,
                    upper_bound: true,
                }
            }
            None => {}
        }
    }
    measure(&planned.item.path, unreadable_is_upper_bound)
}

/// `path`'s freed bytes now, and whether that is only an upper bound. A
/// missing path is 0 without a walk. `unreadable_is_upper_bound` counts a
/// walk that could not read everything as one too: measured after a
/// removal, such a size is too low, so the bytes freed (before minus after)
/// would read too high.
pub(super) fn measure(path: &Path, unreadable_is_upper_bound: bool) -> Measured {
    if matches!(fs::symlink_metadata(path), Err(e) if e.kind() == io::ErrorKind::NotFound) {
        return Measured::default();
    }
    let scan = ScanContext::unchecked();
    let bytes = dir_size_checked(path, &scan);
    let findings = scan.take_findings();
    Measured {
        bytes,
        upper_bound: !findings.size_upper_bound.is_empty()
            || (unreadable_is_upper_bound && !findings.unreadable.is_empty()),
    }
}

/// Measures again right after `outcome` (`measure_again`), given the
/// measurement `before` (`None` when its removal frees nothing or never
/// started). `removed_some` says the route removed at least one entry, which
/// makes a failure partial even when no block was freed (a hard link whose
/// data another link keeps).
pub(super) fn settle(
    measure_again: impl FnOnce() -> Measured,
    outcome: &ItemOutcome,
    before: Option<Measured>,
    removed_some: bool,
) -> Settled {
    let unchanged = |extent| Settled {
        extent,
        freed: 0,
        left: 0,
        upper_bound: false,
    };
    match outcome {
        ItemOutcome::MovedToTrash => return unchanged(RemovalExtent::MovedToTrash),
        ItemOutcome::RemovalRequested => return unchanged(RemovalExtent::Requested),
        ItemOutcome::Skipped(_) | ItemOutcome::NotAttempted => {
            return unchanged(RemovalExtent::NotRemoved)
        }
        _ => {}
    }
    let succeeded = matches!(
        outcome,
        ItemOutcome::Deleted | ItemOutcome::TrashEmptied | ItemOutcome::Pruned { .. }
    );
    let Some(before) = before else {
        let extent = match succeeded {
            true => RemovalExtent::Removed,
            false => RemovalExtent::NotRemoved,
        };
        return unchanged(extent);
    };
    let after = measure_again();
    let freed = before.bytes.saturating_sub(after.bytes);
    let extent = match (succeeded, freed > 0 || removed_some) {
        (true, _) => RemovalExtent::Removed,
        (false, true) => RemovalExtent::PartlyRemoved,
        (false, false) => RemovalExtent::NotRemoved,
    };
    Settled {
        extent,
        freed,
        left: if succeeded { 0 } else { after.bytes },
        upper_bound: before.upper_bound || after.upper_bound,
    }
}
