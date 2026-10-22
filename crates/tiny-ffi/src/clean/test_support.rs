//! Shared fixtures for the cleanup tests: temp dirs, an injected provider and
//! probe, a fake `Trash` and a session whose saved-rules store is a temp dir.
//! Nothing here touches Finder, `osascript` or the real Application Support.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tiny_core::clean::finder_trash::TrashError;
use tiny_core::clean::fs_safe::{dir_size_checked, list_children};
use tiny_core::clean::types::{ExecAction, ExecReport, RiskLevel};
use tiny_core::error::Result as CoreResult;

use super::*;
use crate::FfiProgress;

/// A session whose saved-rules store lives in its own temp directory, so no
/// test ever reads or creates files in the real Application Support.
pub(super) struct TestSession {
    pub(super) session: Arc<TinySession>,
    pub(super) dir: PathBuf,
    owns_dir: bool,
}

impl TestSession {
    pub(super) fn new() -> Self {
        static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let unique = format!(
            "tiny-ffi-protection-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        );
        Self::in_dir(std::env::temp_dir().join(unique), true)
    }

    fn in_dir(dir: PathBuf, owns_dir: bool) -> Self {
        let session = TinySession::new();
        session.clean_state().protection_dir = Some(dir.join("clean-protection"));
        Self {
            session,
            dir,
            owns_dir,
        }
    }

    /// A second session (another window or process) on the same saved rules.
    /// The first session owns the directory and removes it.
    pub(super) fn sibling(&self) -> Self {
        Self::in_dir(self.dir.clone(), false)
    }

    pub(super) fn store(&self) -> ProtectionStore {
        ProtectionStore::at(self.dir.join("clean-protection"))
    }
}

impl std::ops::Deref for TestSession {
    type Target = TinySession;
    fn deref(&self) -> &TinySession {
        &self.session
    }
}

impl Drop for TestSession {
    fn drop(&mut self) {
        if self.owns_dir {
            remove(&self.dir);
        }
    }
}

/// Lists `root`'s children (plus `extra`) as candidates. `execute` panics:
/// the desktop path must act through the `Trash` trait only.
pub(super) struct Fixture {
    pub(super) id: &'static str,
    pub(super) root: PathBuf,
    pub(super) risk: RiskLevel,
    pub(super) desktop: bool,
    pub(super) app: Option<&'static str>,
    pub(super) extra: Vec<PathBuf>,
}

impl Fixture {
    pub(super) fn new(id: &'static str, root: &Path) -> Self {
        Self {
            id,
            root: root.to_path_buf(),
            risk: RiskLevel::Safe,
            desktop: true,
            app: None,
            extra: Vec::new(),
        }
    }
}

impl CleanProvider for Fixture {
    fn id(&self) -> &'static str {
        self.id
    }
    fn label(&self) -> &'static str {
        self.id
    }
    fn risk(&self) -> RiskLevel {
        self.risk
    }
    fn inclusion_reason(&self) -> String {
        "fixture".into()
    }
    fn requires_app_quit(&self) -> Option<&'static str> {
        self.app
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> CoreResult<Vec<CleanItem>> {
        // Extras are category-rooted items, like `root_as_item`.
        for extra in &self.extra {
            ctx.add_root(extra);
        }
        let paths = list_children(&self.root, ctx)
            .into_iter()
            .chain(self.extra.iter().cloned());
        Ok(paths
            .map(|path| CleanItem {
                category_id: self.id.into(),
                category_label: self.id.into(),
                size: dir_size_checked(&path, ctx),
                path,
                risk: self.risk,
                evidence: Vec::new(),
            })
            .collect())
    }
    fn execute(&self, _: &[CleanItem], _: ExecAction) -> CoreResult<ExecReport> {
        panic!("the desktop path must never call provider.execute");
    }
    fn desktop_trash_paths(&self) -> bool {
        self.desktop
    }
}

/// No app is running.
pub(super) struct NotRunning;

impl AppProbe for NotRunning {
    fn probe(&self, _: &str) -> std::result::Result<bool, String> {
        Ok(false)
    }
}

#[derive(Default)]
pub(super) struct Progress(pub(super) Mutex<Vec<FfiProgress>>);

impl ProgressListener for Progress {
    fn on_progress(&self, progress: FfiProgress) {
        self.0.lock().unwrap().push(progress);
    }
}

/// Records moves in memory and deletes nothing. `deny` answers like a
/// Finder Automation denial; `fail_on` fails one path; `panic_on` panics.
#[derive(Default)]
pub(super) struct FakeTrash {
    pub(super) moved: Mutex<Vec<PathBuf>>,
    pub(super) deny: bool,
    pub(super) fail_on: Option<&'static str>,
    pub(super) panic_on: Option<&'static str>,
}

impl Trash for FakeTrash {
    fn move_to_trash(&self, path: &Path) -> std::result::Result<(), TrashError> {
        if self.deny {
            return Err(TrashError::AutomationDenied("(-1743)".into()));
        }
        if self.panic_on.is_some_and(|name| path.ends_with(name)) {
            panic!("simulated panic mid-execute");
        }
        if self.fail_on.is_some_and(|name| path.ends_with(name)) {
            return Err(TrashError::Failed("Finder error".into()));
        }
        self.moved.lock().unwrap().push(path.to_path_buf());
        Ok(())
    }
}

/// A unique temp dir with `names` as children holding `len` bytes each.
pub(super) fn fixture_dir(label: &str, names: &[&str], len: usize) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "tiny-ffi-clean-{label}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    for name in names {
        std::fs::write(dir.join(name), vec![b'x'; len]).unwrap();
    }
    // Test-owned fixtures use physical paths, not macOS's /var temp alias.
    std::fs::canonicalize(dir).unwrap()
}

/// What discovery counts for a test file: its allocated blocks, not its length.
pub(super) fn allocated(path: &Path) -> u64 {
    use std::os::unix::fs::MetadataExt;
    std::fs::symlink_metadata(path).unwrap().blocks() * 512
}

pub(super) fn remove(dir: &Path) {
    let _ = tiny_core::clean::fs_safe::remove_recursive_safe(dir);
}

pub(super) fn discover(
    session: &TinySession,
    providers: &[Box<dyn CleanProvider>],
) -> Result<FfiDiscovery, FfiError> {
    session.discover_clean(
        CleanOptions::default(),
        providers,
        &NotRunning,
        &CancellationToken::default(),
        &Progress::default(),
    )
}

pub(super) fn ids(discovery: &FfiDiscovery) -> Vec<String> {
    discovery
        .categories
        .iter()
        .flat_map(|c| c.candidates.iter().map(|c| c.id.clone()))
        .collect()
}

/// Runs `execute_clean` against a fresh `Fixture("logs", root)` set.
pub(super) fn execute(
    session: &TinySession,
    preview_id: &str,
    root: &Path,
    trash: &FakeTrash,
    token: &CancellationToken,
) -> Result<FfiExecReport, FfiError> {
    let root = root.to_path_buf();
    let providers = move |_: &CleanOptions| -> Vec<Box<dyn CleanProvider>> {
        vec![Box::new(Fixture::new("logs", &root))]
    };
    session.execute_clean(
        preview_id,
        &providers,
        &NotRunning,
        trash,
        token,
        &Progress::default(),
    )
}

/// Discovers `root` as category "logs" and previews every candidate.
pub(super) fn preview_all(session: &TinySession, root: &Path) -> FfiPreview {
    let providers: Vec<Box<dyn CleanProvider>> = vec![Box::new(Fixture::new("logs", root))];
    let discovery = discover(session, &providers).unwrap();
    session.clean_preview(ids(&discovery)).unwrap()
}

pub(super) fn outcomes(report: &FfiExecReport) -> Vec<FfiItemOutcome> {
    report.results.iter().map(|r| r.outcome.clone()).collect()
}

/// `logs` (safe) lists `Logs/*`; `jetbrains` (review) lists `Logs/JetBrains/*`.
pub(super) fn review_inside_safe(dir: &Path) -> Vec<Box<dyn CleanProvider>> {
    let mut jetbrains = Fixture::new("jetbrains", &dir.join("Logs/JetBrains"));
    jetbrains.risk = RiskLevel::Review;
    vec![
        Box::new(Fixture::new("logs", &dir.join("Logs"))),
        Box::new(jetbrains),
    ]
}

pub(super) fn review_fixture(label: &str) -> PathBuf {
    let dir = fixture_dir(label, &[], 0);
    std::fs::create_dir_all(dir.join("Logs/JetBrains/Product")).unwrap();
    std::fs::write(dir.join("Logs/JetBrains/Product/log"), b"12").unwrap();
    std::fs::write(dir.join("Logs/other"), b"1").unwrap();
    dir
}

pub(super) fn candidate_ids(discovery: &FfiDiscovery, category: &str) -> Vec<String> {
    discovery
        .categories
        .iter()
        .filter(|c| c.id == category)
        .flat_map(|c| c.candidates.iter().map(|c| c.id.clone()))
        .collect()
}

// ---------- saved rules ----------

pub(super) fn revision(protection: &FfiProtection) -> u64 {
    match &protection.status {
        FfiProtectionStatus::Available { revision, .. } => *revision,
        other => panic!("protection is not available: {other:?}"),
    }
}

pub(super) fn rules(protection: &FfiProtection) -> Vec<FfiProtectionRule> {
    match &protection.status {
        FfiProtectionStatus::Available { rules, .. } => rules.clone(),
        other => panic!("protection is not available: {other:?}"),
    }
}

/// What the projection says about the candidate whose path ends in `name`
/// (`candidate_id` resolved through the discovery).
pub(super) fn candidate_protection(
    protection: &FfiProtection,
    discovery: &FfiDiscovery,
    name: &str,
) -> FfiCandidateProtection {
    let id = id_of(discovery, name);
    match &protection.status {
        FfiProtectionStatus::Available { candidates, .. } => candidates
            .iter()
            .find(|c| c.candidate_id == id)
            .unwrap_or_else(|| panic!("no projection for {name}"))
            .clone(),
        other => panic!("protection is not available: {other:?}"),
    }
}

/// The candidate ID whose path ends in `name`.
pub(super) fn id_of(discovery: &FfiDiscovery, name: &str) -> String {
    let found = discovery
        .categories
        .iter()
        .flat_map(|c| &c.candidates)
        .find(|c| c.path.ends_with(name));
    found
        .unwrap_or_else(|| panic!("no candidate {name}"))
        .id
        .clone()
}

/// Loads the rules, then keeps `id` at the revision just loaded.
pub(super) fn keep_candidate(session: &TinySession, id: &str) -> FfiProtection {
    let revision = revision(&session.clean_protection().unwrap());
    session.clean_keep(id.into(), revision).unwrap()
}
