//! State shared by one discovery run: cancellation, the running-app probe,
//! and what the walks could not read or where they started.
//!
//! Providers receive it in `discover`; the checked discovery drains the
//! findings after each provider so they are attributed to that category.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

use crate::engine_error;
use crate::error::Result;

use super::freed_size::SharedBlocks;
use super::process::{AppProbe, PgrepChecker};

/// Entries under one walked path that could not be read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unreadable {
    pub entries: u64,
    /// First error seen, for diagnostics.
    pub first_error: String,
}

/// What the walks recorded while one provider ran.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ScanFindings {
    /// Keyed by the path the walk started from (an item or a search root).
    pub unreadable: BTreeMap<PathBuf, Unreadable>,
    /// Directories discovery listed or walked; execution only acts inside them.
    pub roots: BTreeSet<PathBuf>,
    /// Items whose size counts blocks the volume could not show as unshared,
    /// so deleting one may free less.
    pub size_upper_bound: BTreeSet<PathBuf>,
    /// Blocks each item shares with files outside it, keyed like
    /// `size_upper_bound`.
    pub shared: BTreeMap<PathBuf, SharedBlocks>,
    /// Paths found but never offered because a safety check could not
    /// answer, with why.
    pub refused: BTreeMap<PathBuf, String>,
}

impl ScanFindings {
    /// Folds what a worker recorded into `self`, leaving exactly what one
    /// context would hold had the worker's walks run on it: counts add up and
    /// the first error already seen is kept.
    pub fn absorb(&mut self, other: ScanFindings) {
        for (path, theirs) in other.unreadable {
            self.unreadable
                .entry(path)
                .and_modify(|ours| ours.entries += theirs.entries)
                .or_insert(theirs);
        }
        self.roots.extend(other.roots);
        self.size_upper_bound.extend(other.size_upper_bound);
        self.shared.extend(other.shared);
        self.refused.extend(other.refused);
    }
}

/// The probe of a worker context: workers never decide anything about
/// running apps, so a call is a bug and must be visible.
struct NoProbe;

impl AppProbe for NoProbe {
    fn probe(&self, name: &str) -> std::result::Result<bool, String> {
        Err(format!("worker contexts cannot probe for {name}"))
    }
}

static WORKER_PROBE: NoProbe = NoProbe;

pub struct ScanContext<'a> {
    cancel: Option<&'a AtomicBool>,
    probe: &'a dyn AppProbe,
    /// Set by the desktop only: refuse protected paths at discovery.
    protected_home: Option<PathBuf>,
    findings: RefCell<ScanFindings>,
}

static LEGACY_PROBE: PgrepChecker = PgrepChecker;

impl<'a> ScanContext<'a> {
    pub fn new(cancel: Option<&'a AtomicBool>, probe: &'a dyn AppProbe) -> Self {
        Self {
            cancel,
            probe,
            protected_home: None,
            findings: RefCell::new(ScanFindings::default()),
        }
    }

    /// Desktop discovery: leave out `/`, `home`, its ancestors and
    /// tool-printed paths outside `home` (`trash_plan::protected_reason`).
    /// The CLI never sets this, so it lists exactly what it always did.
    pub fn refusing_protected_paths(self, home: PathBuf) -> Self {
        Self {
            protected_home: Some(home),
            ..self
        }
    }

    pub fn protected_home(&self) -> Option<&Path> {
        self.protected_home.as_deref()
    }

    /// The CLI's context: never cancelled, and a failed `pgrep` counts as
    /// "not running", exactly as before checked discovery existed.
    pub fn unchecked() -> ScanContext<'static> {
        ScanContext::new(None, &LEGACY_PROBE)
    }

    /// The flag a worker thread needs to build its own [`Self::worker`].
    /// The context itself is not `Sync`, but the flag is.
    pub fn cancel_flag(&self) -> Option<&'a AtomicBool> {
        self.cancel
    }

    /// A context for one parallel task: it shares the cancel flag, records
    /// into findings of its own and answers no app probe. Hand its
    /// `take_findings` back to the parent through [`Self::absorb`].
    pub fn worker(cancel: Option<&'a AtomicBool>) -> Self {
        Self::new(cancel, &WORKER_PROBE)
    }

    /// Records what a worker context found, as if it had run here.
    pub fn absorb(&self, findings: ScanFindings) {
        self.findings.borrow_mut().absorb(findings);
    }

    pub fn is_cancelled(&self) -> bool {
        self.cancel.is_some_and(|flag| flag.load(Ordering::Acquire))
    }

    /// Whether `app` is running. A failed probe is an error, never "not running".
    pub fn app_running(&self, app: &str) -> Result<bool> {
        self.probe
            .probe(app)
            .map_err(|detail| engine_error!("running-app check for {app} failed: {detail}"))
    }

    /// Per-path gate during discovery: skip a path only when its app is
    /// known to be running. An unknown answer keeps the path as a
    /// candidate; execution re-checks it and refuses it if still unknown,
    /// so one failed probe never hides the rest of the category.
    pub fn known_running(&self, app: &str) -> bool {
        self.probe.probe(app) == Ok(true)
    }

    pub fn probe(&self) -> &dyn AppProbe {
        self.probe
    }

    /// Records a read failure under `walk_root`. A missing path is not a
    /// limitation: most providers probe paths that may not exist.
    pub fn note_unreadable(&self, walk_root: &Path, error: &io::Error) {
        if error.kind() == io::ErrorKind::NotFound {
            return;
        }
        self.findings
            .borrow_mut()
            .unreadable
            .entry(walk_root.to_path_buf())
            .and_modify(|u| u.entries += 1)
            .or_insert_with(|| Unreadable {
                entries: 1,
                first_error: error.to_string(),
            });
    }

    /// Records a path left out because a safety check could not answer, so
    /// the category says why instead of looking empty.
    pub fn note_refused(&self, path: &Path, reason: String) {
        self.findings
            .borrow_mut()
            .refused
            .insert(path.to_path_buf(), reason);
    }

    pub fn note_size_upper_bound(&self, item: &Path) {
        self.findings
            .borrow_mut()
            .size_upper_bound
            .insert(item.to_path_buf());
    }

    pub fn note_shared_blocks(&self, item: &Path, shared: SharedBlocks) {
        self.findings
            .borrow_mut()
            .shared
            .insert(item.to_path_buf(), shared);
    }

    pub fn add_root(&self, root: &Path) {
        self.findings.borrow_mut().roots.insert(root.to_path_buf());
    }

    pub fn take_findings(&self) -> ScanFindings {
        self.findings.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::freed_size::SharedBlocks;
    use crate::clean::process::test_support::{FailingProbe, MockChecker};

    #[test]
    fn missing_paths_are_not_limitations() {
        let ctx = ScanContext::unchecked();
        ctx.note_unreadable(Path::new("/a"), &io::ErrorKind::NotFound.into());
        ctx.note_unreadable(Path::new("/a"), &io::ErrorKind::PermissionDenied.into());
        ctx.note_unreadable(Path::new("/a"), &io::ErrorKind::PermissionDenied.into());
        let findings = ctx.take_findings();
        assert_eq!(findings.unreadable[Path::new("/a")].entries, 2);
        assert!(ctx.take_findings().unreadable.is_empty(), "take drains");
    }

    #[test]
    fn cancellation_follows_the_flag() {
        let flag = AtomicBool::new(false);
        let mock = MockChecker::none();
        let ctx = ScanContext::new(Some(&flag), &mock);
        assert!(!ctx.is_cancelled());
        flag.store(true, Ordering::Release);
        assert!(ctx.is_cancelled());
    }

    #[test]
    fn worker_shares_the_flag_records_apart_and_never_probes() {
        let flag = AtomicBool::new(false);
        let mock = MockChecker::none();
        let parent = ScanContext::new(Some(&flag), &mock);
        let worker = ScanContext::worker(parent.cancel_flag());
        worker.note_unreadable(Path::new("/a"), &io::ErrorKind::PermissionDenied.into());
        assert!(parent.take_findings().unreadable.is_empty());
        assert!(worker.app_running("Xcode").is_err());
        assert!(!worker.known_running("Xcode"));
        flag.store(true, Ordering::Release);
        assert!(worker.is_cancelled());
    }

    #[test]
    fn absorb_adds_counts_and_keeps_the_first_error() {
        let denied = || io::Error::from(io::ErrorKind::PermissionDenied);
        let other = || io::Error::other("later");
        let parent = ScanContext::unchecked();
        parent.note_unreadable(Path::new("/a"), &denied());
        parent.add_root(Path::new("/r1"));
        let worker = ScanContext::worker(None);
        worker.note_unreadable(Path::new("/a"), &other());
        worker.note_unreadable(Path::new("/a"), &other());
        worker.note_unreadable(Path::new("/b"), &other());
        worker.add_root(Path::new("/r2"));
        worker.note_size_upper_bound(Path::new("/b"));
        worker.note_shared_blocks(Path::new("/b"), SharedBlocks::default());
        worker.note_refused(Path::new("/c"), "why".into());
        // The same calls on one context are the reference.
        let one = ScanContext::unchecked();
        one.note_unreadable(Path::new("/a"), &denied());
        one.add_root(Path::new("/r1"));
        one.note_unreadable(Path::new("/a"), &other());
        one.note_unreadable(Path::new("/a"), &other());
        one.note_unreadable(Path::new("/b"), &other());
        one.add_root(Path::new("/r2"));
        one.note_size_upper_bound(Path::new("/b"));
        one.note_shared_blocks(Path::new("/b"), SharedBlocks::default());
        one.note_refused(Path::new("/c"), "why".into());
        parent.absorb(worker.take_findings());
        let merged = parent.take_findings();
        assert_eq!(merged, one.take_findings());
        assert_eq!(merged.unreadable[Path::new("/a")].entries, 3);
        assert_eq!(
            merged.unreadable[Path::new("/a")].first_error,
            denied().to_string()
        );
    }

    #[test]
    fn failed_probe_is_an_error_not_not_running() {
        let ctx = ScanContext::new(None, &FailingProbe);
        assert!(ctx.app_running("Xcode").is_err());
    }
}
