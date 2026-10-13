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
}

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
    fn failed_probe_is_an_error_not_not_running() {
        let ctx = ScanContext::new(None, &FailingProbe);
        assert!(ctx.app_running("Xcode").is_err());
    }
}
