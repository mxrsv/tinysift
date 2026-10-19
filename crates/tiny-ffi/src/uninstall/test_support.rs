//! Fixtures for the uninstall tests: temp directories, a fake inventory,
//! probe and `Trash`, and a temp saved-rules store. Nothing here calls
//! Finder, reads `/Applications` or the real Application Support, or moves
//! a real file.

use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use tiny_core::clean::finder_trash::TrashError;
use tiny_core::clean::protection::RuleScope;
use tiny_core::clean::protection_store::ProtectionStore;

use super::*;
use crate::clean::FfiItemOutcome;
use crate::FfiProgress;

pub(super) const ID: &str = "com.example.Fixture";
pub(super) const EXE: &str = "FixtureExec";
pub(super) const APP: &str = "Fixture App";

#[derive(Default)]
pub(super) struct Listener(pub(super) Mutex<Vec<FfiProgress>>);

impl ProgressListener for Listener {
    fn on_progress(&self, progress: FfiProgress) {
        self.0.lock().unwrap().push(progress);
    }
}

pub(super) struct FakeProbe {
    pub(super) running: AtomicBool,
    pub(super) fails: AtomicBool,
}

impl AppProbe for FakeProbe {
    fn probe(&self, name: &str) -> Result<bool, String> {
        assert_eq!(name, EXE);
        if self.fails.load(Ordering::SeqCst) {
            return Err("probe unavailable".into());
        }
        Ok(self.running.load(Ordering::SeqCst))
    }
}

/// An inventory of fixture apps; `unreadable` makes `identities` fail.
pub(super) struct FakeCatalog {
    pub(super) root: PathBuf,
    pub(super) apps: Mutex<Vec<AppEntry>>,
    pub(super) unreadable: AtomicBool,
    /// `None` makes the process name unknown.
    pub(super) executable: Mutex<Option<String>>,
}

impl AppCatalog for FakeCatalog {
    fn applications_root(&self) -> &Path {
        &self.root
    }
    fn list(&self) -> Result<Vec<AppEntry>, FfiError> {
        Ok(self.apps.lock().unwrap().clone())
    }
    fn identities(&self) -> Result<Vec<AppEntry>, FfiError> {
        if self.unreadable.load(Ordering::SeqCst) {
            return Err(FfiError::Io {
                detail: "unreadable".into(),
            });
        }
        self.list()
    }
    fn executable(&self, _app: &Path) -> Option<String> {
        self.executable.lock().unwrap().clone()
    }
}

pub(super) type MoveHook<'a> = Box<dyn Fn(&Path) + Send + Sync + 'a>;

/// Records each move without touching the disk. `on_move` runs first.
#[derive(Default)]
pub(super) struct FakeTrash<'a> {
    pub(super) moved: Mutex<Vec<PathBuf>>,
    pub(super) fail_on: Option<(&'static str, TrashError)>,
    pub(super) on_move: Option<MoveHook<'a>>,
}

impl FakeTrash<'_> {
    pub(super) fn moved_names(&self) -> Vec<String> {
        let moved = self.moved.lock().unwrap();
        moved.iter().map(|p| file_name(p)).collect()
    }
}

impl Trash for FakeTrash<'_> {
    fn move_to_trash(&self, path: &Path) -> Result<(), TrashError> {
        if let Some(hook) = &self.on_move {
            hook(path);
        }
        if let Some((name, error)) = &self.fail_on {
            if path.ends_with(name) {
                return Err(error.clone());
            }
        }
        self.moved.lock().unwrap().push(path.to_path_buf());
        Ok(())
    }
}

pub(super) fn file_name(path: &Path) -> String {
    path.file_name().unwrap().to_string_lossy().into_owned()
}

pub(super) struct World {
    pub(super) root: PathBuf,
    pub(super) home: PathBuf,
    pub(super) store_dir: PathBuf,
    pub(super) session: Arc<TinySession>,
    pub(super) catalog: FakeCatalog,
    pub(super) probe: FakeProbe,
    pub(super) listener: Listener,
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

pub(super) fn app_entry(root: &Path, name: &str, id: Option<&str>) -> AppEntry {
    AppEntry {
        name: name.into(),
        path: root.join(format!("{name}.app")),
        bundle_id: id.map(String::from),
        size: 4,
        last_used_days: Some(3),
    }
}

impl World {
    /// A home with a cache, app data, a preference file, a symlinked
    /// storage entry (uncertain) and a group container (report-only), plus
    /// installed `Fixture App` and a blocked system app.
    pub(super) fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let tmp = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let root = tmp.join(format!(
            "tiny-ffi-uninstall-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let (home, apps) = (root.join("home"), root.join("apps"));
        let lib = home.join("Library");
        for dir in [
            apps.join(format!("{APP}.app/Contents")),
            lib.join("Caches").join(ID),
            lib.join("Application Support").join(ID),
            lib.join("Group Containers").join(format!("group.{ID}")),
            lib.join("HTTPStorages"),
            lib.join("Preferences"),
            root.join("elsewhere"),
        ] {
            std::fs::create_dir_all(dir).unwrap();
        }
        std::fs::write(lib.join("Caches").join(ID).join("blob"), b"1234").unwrap();
        std::fs::write(lib.join("Preferences").join(format!("{ID}.plist")), b"x").unwrap();
        symlink(root.join("elsewhere"), lib.join("HTTPStorages").join(ID)).unwrap();
        let session = TinySession::new();
        let store_dir = root.join("store/clean-protection");
        session.inject_clean_inputs(&home, &home, store_dir.clone());
        let installed = vec![
            app_entry(&apps, APP, Some(ID)),
            app_entry(&apps, "Zed Fixture System", Some("com.apple.fixture")),
        ];
        Self {
            catalog: FakeCatalog {
                root: apps,
                apps: Mutex::new(installed),
                unreadable: AtomicBool::new(false),
                executable: Mutex::new(Some(EXE.into())),
            },
            probe: FakeProbe {
                running: AtomicBool::new(false),
                fails: AtomicBool::new(false),
            },
            listener: Listener::default(),
            root,
            home,
            store_dir,
            session,
        }
    }

    pub(super) fn app_id(&self) -> String {
        let state = self.session.uninstall_state();
        let listing = state.listing.as_ref().unwrap();
        let found = listing.apps.iter().find(|(_, app)| app.name == APP);
        found.unwrap().0.clone()
    }

    pub(super) fn lib(&self, rel: &str) -> PathBuf {
        self.home.join("Library").join(rel)
    }

    pub(super) fn list(&self) -> Vec<FfiInstalledApp> {
        self.session.list_installed_apps(&self.catalog).unwrap()
    }

    pub(super) fn preview(&self) -> FfiUninstallPreview {
        let apps = self.list();
        let app = apps.iter().find(|a| a.name == APP).unwrap();
        self.session
            .preview_uninstall(&app.app_id, &self.catalog, &self.probe)
            .unwrap()
    }

    pub(super) fn execute(
        &self,
        trash: &FakeTrash<'_>,
        preview_id: &str,
        items: &[&str],
        token: &CancellationToken,
    ) -> Result<FfiUninstallReport, FfiError> {
        let seams = Seams {
            catalog: &self.catalog,
            probe: &self.probe,
            trash,
        };
        let ids: Vec<String> = items.iter().map(|s| s.to_string()).collect();
        self.session
            .execute_uninstall(preview_id, &ids, &seams, token, &self.listener)
    }

    /// Runs `names` (file names found in the preview) with a fresh token.
    pub(super) fn run(
        &self,
        trash: &FakeTrash<'_>,
        preview: &FfiUninstallPreview,
        names: &[&str],
    ) -> Result<FfiUninstallReport, FfiError> {
        let ids: Vec<&str> = names
            .iter()
            .map(|n| item(preview, n).item_id.as_str())
            .collect();
        self.execute(
            trash,
            &preview.preview_id,
            &ids,
            &CancellationToken::default(),
        )
    }

    pub(super) fn save_rule(&self, path: &Path) {
        let store = ProtectionStore::at(self.store_dir.clone());
        let mut lease = store.lease().unwrap();
        let revision = lease.snapshot().revision;
        lease.add(revision, path, RuleScope::Directory).unwrap();
    }
}

pub(super) fn item<'a>(preview: &'a FfiUninstallPreview, name: &str) -> &'a FfiUninstallItem {
    preview
        .items
        .iter()
        .find(|i| i.path.ends_with(name))
        .unwrap_or_else(|| panic!("no item {name}"))
}

pub(super) fn outcome<'a>(
    report: &'a FfiUninstallReport,
    preview: &FfiUninstallPreview,
    name: &str,
) -> &'a FfiItemOutcome {
    let id = &item(preview, name).item_id;
    &report
        .results
        .iter()
        .find(|r| &r.item_id == id)
        .unwrap()
        .outcome
}

pub(super) fn expect_refusal(error: FfiError) -> FfiUninstallRefusal {
    match error {
        FfiError::UninstallRefused { refusal } => refusal,
        other => panic!("expected an uninstall refusal, got {other:?}"),
    }
}

pub(super) const BUNDLE: &str = "Fixture App.app";
pub(super) const CACHE_NAME: &str = "com.example.Fixture";
pub(super) const PREFS: &str = "com.example.Fixture.plist";
pub(super) const GROUP: &str = "group.com.example.Fixture";
