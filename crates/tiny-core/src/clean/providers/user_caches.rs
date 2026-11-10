use crate::error::Result;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use super::purged_caches::{real_pip, PIP_FOLDER};
use super::{execute_per_item, run_tool, CleanProvider};
use crate::clean::fs_safe::{dir_size_checked, is_dir_safe, list_children};
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, Evidence, ExecAction, ExecReport, RiskLevel};
use crate::runner::CommandRunner;

const ID: &str = "user-caches";
const LABEL: &str = "User caches";

/// Absolute paths: a Finder-launched app has a minimal `PATH`.
const MDFIND: &str = "/usr/bin/mdfind";
const PLUTIL: &str = "/usr/bin/plutil";
const BUNDLE_ID_ATTR: &str = "kMDItemCFBundleIdentifier";
const APPLE_PREFIX: &str = "com.apple.";
/// Apple caches whose folder name lacks the `com.apple.` prefix.
const APPLE_FOLDERS: &[&str] = &[
    "Animoji",
    "CloudKit",
    "FamilyCircle",
    "familycircled",
    "GeoServices",
    "PassKit",
];
/// Folders named after a vendor, not a bundle id, and the app that owns them.
const VENDOR_APPS: &[(&str, &str)] = &[
    ("Google", "Google Chrome"),
    ("BraveSoftware", "Brave Browser"),
];
/// Where a JetBrains-platform IDE keeps the user's edit history, inside its
/// system directory: `<vendor>/<product><version>`, or `<product><version>`
/// alone for older IDEs.
const LOCAL_HISTORY: &str = "LocalHistory";
/// The vendor folder `jetbrains` lists what the IDEs rebuild from.
const JETBRAINS: &str = "JetBrains";
/// Ids per Spotlight query, so hundreds of cache folders stay safe.
const QUERY_CHUNK: usize = 50;

/// The app that owns a cache folder.
#[derive(Clone)]
struct Owner {
    /// `CFBundleExecutable`, the process name `pgrep -x` matches.
    executable: String,
    /// Found through the parent id (`X.ShipIt` -> `X`), not the folder's own.
    via_parent: bool,
}

pub struct UserCaches {
    runner: Arc<dyn CommandRunner>,
    /// Folder name -> owning app (`None`: no app resolved). Filled once per
    /// discovery, so `item_apps` at execution time does not spawn again.
    owners: Mutex<HashMap<String, Option<Owner>>>,
}

impl UserCaches {
    pub fn with_runner(runner: Arc<dyn CommandRunner>) -> Self {
        Self {
            runner,
            owners: Mutex::new(HashMap::new()),
        }
    }

    fn cached_owner(&self, id: &str) -> Option<Option<Owner>> {
        let owners = self.owners.lock().unwrap_or_else(|e| e.into_inner());
        owners.get(id).cloned()
    }

    /// Resolves every id (and its parent id, for `X.ShipIt`-style helper
    /// folders) with batched Spotlight queries plus one `plutil` per matched
    /// app, and replaces their cache entries. A failed tool leaves the
    /// affected ids unresolved; it never fails the category.
    fn resolve(&self, ids: &[&str]) {
        let mut queried: Vec<&str> = ids
            .iter()
            .copied()
            .chain(ids.iter().filter_map(|id| parent_id(id)))
            .collect();
        queried.sort_unstable();
        queried.dedup();
        let found = self.find_apps(&queried);
        // One `plutil` per queried id, shared by the folder and its helpers.
        let mut executables: HashMap<&str, Option<String>> = HashMap::new();
        let mut executable_of_id = |id| {
            executables
                .entry(id)
                .or_insert_with(|| {
                    let apps = found.get(id)?;
                    apps.iter().find_map(|app| self.executable_of(app))
                })
                .clone()
        };
        let resolved: Vec<_> = ids
            .iter()
            .map(|id| {
                let owner = match executable_of_id(id) {
                    Some(executable) => Some((executable, false)),
                    None => parent_id(id)
                        .and_then(&mut executable_of_id)
                        .map(|executable| (executable, true)),
                };
                let owner = owner.map(|(executable, via_parent)| Owner {
                    executable,
                    via_parent,
                });
                ((*id).to_string(), owner)
            })
            .collect();
        let mut owners = self.owners.lock().unwrap_or_else(|e| e.into_inner());
        owners.extend(resolved);
    }

    /// Bundle id -> paths of the `.app` bundles Spotlight lists for it.
    /// `ids` are validated by the caller, so they are safe to put in the query.
    fn find_apps(&self, ids: &[&str]) -> HashMap<String, Vec<String>> {
        let mut found: HashMap<String, Vec<String>> = HashMap::new();
        for chunk in ids.chunks(QUERY_CHUNK) {
            let query = chunk
                .iter()
                .map(|id| format!("{BUNDLE_ID_ATTR} == '{id}'"))
                .collect::<Vec<_>>()
                .join(" || ");
            let out = match run_tool(
                self.runner.as_ref(),
                MDFIND,
                &["-attr", BUNDLE_ID_ATTR, &query],
            ) {
                Ok(out) if out.success() => out,
                _ => continue,
            };
            let marker = format!(" {BUNDLE_ID_ATTR} = ");
            for line in out.stdout.lines() {
                let Some((app, id)) = line.rsplit_once(&marker) else {
                    continue;
                };
                let id = id.trim();
                if chunk.contains(&id) {
                    found
                        .entry(id.to_string())
                        .or_default()
                        .push(app.trim().to_string());
                }
            }
        }
        found
    }

    /// `CFBundleExecutable`: the process name `pgrep -x` matches (VS Code is
    /// `Code`, not `VSCode`).
    fn executable_of(&self, app: &str) -> Option<String> {
        let plist = format!("{app}/Contents/Info.plist");
        let out = run_tool(
            self.runner.as_ref(),
            PLUTIL,
            &["-extract", "CFBundleExecutable", "raw", "-o", "-", &plist],
        )
        .ok()?;
        let exe = out.stdout.trim();
        (out.success() && !exe.is_empty()).then(|| exe.to_string())
    }

    /// The resolved owner of a cache folder name, resolving lazily
    /// when discovery has not seen it (a fresh provider at execution time).
    /// A vendor folder is owned by the app in `VENDOR_APPS`.
    fn owner(&self, name: &str) -> Option<Owner> {
        if let Some((_, app)) = VENDOR_APPS.iter().find(|(folder, _)| *folder == name) {
            return Some(Owner {
                executable: (*app).to_string(),
                via_parent: false,
            });
        }
        if !is_bundle_id(name) {
            return None;
        }
        if let Some(cached) = self.cached_owner(name) {
            return cached;
        }
        self.resolve(&[name]);
        self.cached_owner(name).flatten()
    }

    /// Process names to check for a cache folder: the resolved executable,
    /// plus the last-segment guess for a helper folder found through its
    /// parent, and the guess alone when nothing resolved, so the gate is
    /// never empty.
    fn gate_apps(&self, name: &str) -> Vec<String> {
        let guess = guess_app(name).map(String::from);
        match self.owner(name) {
            Some(Owner {
                executable,
                via_parent: false,
            }) => vec![executable],
            Some(Owner { executable, .. }) => {
                let guess = guess.filter(|g| *g != executable);
                std::iter::once(executable).chain(guess).collect()
            }
            None => guess.into_iter().collect(),
        }
    }

    fn list_caches(&self, ctx: &ScanContext<'_>, root: &Path) -> Result<Vec<CleanItem>> {
        // pip's folder is `pip-cache`'s when a real pip3 can empty it, so
        // one path never belongs to a tool and to a plain move at once.
        let pip_owned = real_pip(self.runner.as_ref()).is_some();
        let owned = |name: &str| pip_owned && name == PIP_FOLDER;
        let paths: Vec<(PathBuf, String)> = list_children(root, ctx)
            .into_iter()
            .filter_map(|path| {
                let name = path.file_name()?.to_str()?.to_string();
                let offered = !is_apple_cache(&name) && !owned(&name) && !holds_ide_history(&path);
                offered.then_some((path, name))
            })
            .collect();
        let ids: Vec<&str> = paths
            .iter()
            .map(|(_, name)| name.as_str())
            .filter(|name| is_bundle_id(name))
            .collect();
        self.resolve(&ids);
        let mut out = Vec::new();
        for (path, name) in paths {
            if self.gate_apps(&name).iter().any(|a| ctx.known_running(a)) {
                continue;
            }
            let evidence = match self.owner(&name) {
                Some(owner) => Evidence::OwningApp {
                    name: owner.executable,
                },
                None => Evidence::OwningAppUnknown,
            };
            let size = dir_size_checked(&path, ctx);
            out.push(CleanItem {
                category_id: ID.to_string(),
                category_label: LABEL.to_string(),
                path,
                size,
                risk: RiskLevel::Review,
                evidence: vec![evidence],
            });
        }
        Ok(out)
    }
}

impl CleanProvider for UserCaches {
    fn id(&self) -> &'static str {
        ID
    }
    fn label(&self) -> &'static str {
        LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Per-app caches in ~/Library/Caches that apps rebuild".into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    fn desktop_trash_paths(&self) -> bool {
        true
    }
    fn item_apps(&self, path: &Path) -> Vec<String> {
        match path.file_name().and_then(|s| s.to_str()) {
            Some(name) => self.gate_apps(name),
            None => Vec::new(),
        }
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let h = match home() {
            Some(h) => h,
            None => return Ok(Vec::new()),
        };
        self.list_caches(ctx, &h.join("Library/Caches"))
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_per_item(items, action, ID)
    }
}

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

/// An IDE's system directory, or a vendor folder with one in it, is never
/// offered whole: beside caches that holds Local History, the user's edit
/// history, which nothing rebuilds. JetBrains' own is `jetbrains`'s to list;
/// any other (`Google/AndroidStudio*`) is found by the history folder itself.
// ponytail: the vendor's other caches (`Google/Chrome`) go unoffered with
// it; list such a folder's children one by one if that space is missed.
fn holds_ide_history(vendor: &Path) -> bool {
    vendor.file_name().is_some_and(|name| name == JETBRAINS)
        || is_dir_safe(&vendor.join(LOCAL_HISTORY))
        || std::fs::read_dir(vendor).is_ok_and(|systems| {
            systems
                .flatten()
                .any(|system| is_dir_safe(&system.path().join(LOCAL_HISTORY)))
        })
}

/// System daemon caches (`com.apple.bird`, `CloudKit`, ...) are not the
/// user's to clear.
fn is_apple_cache(name: &str) -> bool {
    name.get(..APPLE_PREFIX.len())
        .is_some_and(|p| p.eq_ignore_ascii_case(APPLE_PREFIX))
        || APPLE_FOLDERS.iter().any(|f| f.eq_ignore_ascii_case(name))
}

/// Looks like a reverse-DNS bundle id. Checked before the name goes into a
/// Spotlight query, so only `[A-Za-z0-9.-]` ever reaches it.
fn is_bundle_id(name: &str) -> bool {
    name.contains('.')
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-')
}

/// `com.microsoft.VSCode.ShipIt` -> `com.microsoft.VSCode`: one level up,
/// where a helper folder's app usually lives.
fn parent_id(id: &str) -> Option<&str> {
    id.rsplit_once('.')
        .map(|(parent, _)| parent)
        .filter(|parent| is_bundle_id(parent))
}

/// Fallback when no app resolves: the *last* dot segment of the folder name
/// (`pgrep -x` target) — best-effort, not airtight.
fn guess_app(name: &str) -> Option<&str> {
    let last_segment = name.rsplit('.').next().unwrap_or(name);
    (!last_segment.is_empty()).then_some(last_segment)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::process::test_support::MockChecker;
    use crate::runner::test_support::MockRunner;

    fn fixture(label: &str, names: &[&str]) -> PathBuf {
        let root =
            std::env::temp_dir().join(format!("tiny-user-caches-{label}-{}", std::process::id()));
        for name in names {
            std::fs::create_dir_all(root.join(name)).unwrap();
        }
        root
    }

    fn cleanup(root: &Path) {
        let _ = crate::clean::fs_safe::remove_recursive_safe(root);
    }

    /// The batched query for `ids`, as the provider builds it.
    fn query(ids: &[&str]) -> String {
        let clauses: Vec<_> = ids
            .iter()
            .map(|id| format!("kMDItemCFBundleIdentifier == '{id}'"))
            .collect();
        clauses.join(" || ")
    }

    /// Mocks the batched query for `ids` and one `plutil` read for VS Code.
    fn vscode_runner_for(ids: &[&str]) -> MockRunner {
        let query = query(ids);
        MockRunner::new()
            .with_exit(
                MDFIND,
                &["-attr", "kMDItemCFBundleIdentifier", &query],
                0,
                "/Applications/Visual Studio Code.app   kMDItemCFBundleIdentifier = com.microsoft.VSCode\n",
            )
            .with_exit(
                PLUTIL,
                &[
                    "-extract",
                    "CFBundleExecutable",
                    "raw",
                    "-o",
                    "-",
                    "/Applications/Visual Studio Code.app/Contents/Info.plist",
                ],
                0,
                "Code\n",
            )
    }

    /// VS Code's own folder: the queried ids are it and its parent.
    fn vscode_runner() -> MockRunner {
        vscode_runner_for(&["com.microsoft", "com.microsoft.VSCode"])
    }

    fn provider(runner: MockRunner) -> UserCaches {
        UserCaches::with_runner(Arc::new(runner))
    }

    fn names(items: &[CleanItem]) -> Vec<String> {
        items
            .iter()
            .map(|i| i.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect()
    }

    #[test]
    fn apple_daemon_caches_are_never_offered() {
        let root = fixture(
            "apple",
            &[
                "com.apple.bird",
                "COM.APPLE.passd",
                "Animoji",
                "CloudKit",
                "FamilyCircle",
                "familycircled",
                "GeoServices",
                "PassKit",
                "Homebrew",
            ],
        );
        let m = MockChecker::none();
        let ctx = ScanContext::new(None, &m);
        let items = provider(MockRunner::new())
            .list_caches(&ctx, &root)
            .unwrap();
        assert_eq!(names(&items), vec!["Homebrew"]);
        cleanup(&root);
    }

    #[test]
    fn a_vendor_folder_holding_an_ides_local_history_is_never_offered() {
        let root = fixture(
            "ide-history",
            &["JetBrains", "Google", "Homebrew", "IdeaIC2019.3"],
        );
        let history = root.join("Google/AndroidStudio2025.1/LocalHistory");
        std::fs::create_dir_all(&history).unwrap();
        std::fs::create_dir_all(root.join("IdeaIC2019.3/LocalHistory")).unwrap();
        std::fs::create_dir_all(root.join("Google/Chrome")).unwrap();
        let m = MockChecker::none();
        let ctx = ScanContext::new(None, &m);
        let caches = provider(MockRunner::new());
        assert_eq!(
            names(&caches.list_caches(&ctx, &root).unwrap()),
            ["Homebrew"]
        );
        // Without an IDE's history in it, the vendor's folder is a cache like any other.
        std::fs::remove_dir(&history).unwrap();
        assert_eq!(
            names(&caches.list_caches(&ctx, &root).unwrap()),
            ["Google", "Homebrew"]
        );
        cleanup(&root);
    }

    #[test]
    fn a_vendor_folder_is_gated_by_the_vendor_app() {
        let root = fixture("vendor", &["Google", "BraveSoftware"]);
        let chrome = MockChecker::with_running(["Google Chrome"]);
        let ctx = ScanContext::new(None, &chrome);
        let caches = provider(MockRunner::new());
        let items = caches.list_caches(&ctx, &root).unwrap();
        assert_eq!(names(&items), vec!["BraveSoftware"], "Chrome is running");
        assert_eq!(
            items[0].evidence,
            vec![Evidence::OwningApp {
                name: "Brave Browser".into()
            }]
        );
        let idle = MockChecker::none();
        let ctx = ScanContext::new(None, &idle);
        let items = caches.list_caches(&ctx, &root).unwrap();
        let google = items.iter().find(|i| i.path.ends_with("Google")).unwrap();
        assert_eq!(
            google.evidence,
            vec![Evidence::OwningApp {
                name: "Google Chrome".into()
            }]
        );
        assert_eq!(caches.item_apps(&google.path), vec!["Google Chrome"]);
        cleanup(&root);
    }

    #[test]
    fn a_resolved_executable_gates_the_cache_while_the_app_runs() {
        let root = fixture("running", &["com.microsoft.VSCode"]);
        let m = MockChecker::with_running(["Code"]);
        let ctx = ScanContext::new(None, &m);
        let items = provider(vscode_runner()).list_caches(&ctx, &root).unwrap();
        assert!(items.is_empty(), "Code is running");
        assert!(ctx.take_findings().roots.contains(&root));
        cleanup(&root);
    }

    #[test]
    fn a_resolved_app_that_is_idle_is_offered_with_its_executable() {
        let root = fixture("idle", &["com.microsoft.VSCode"]);
        let m = MockChecker::with_running(["VSCode"]);
        let ctx = ScanContext::new(None, &m);
        let items = provider(vscode_runner()).list_caches(&ctx, &root).unwrap();
        assert_eq!(names(&items), vec!["com.microsoft.VSCode"]);
        assert_eq!(
            items[0].evidence,
            vec![Evidence::OwningApp {
                name: "Code".into()
            }]
        );
        cleanup(&root);
    }

    #[test]
    fn an_unresolved_name_is_offered_unknown_and_gated_by_the_guess() {
        let root = fixture("unknown", &["com.example.Idle", "com.example.Busy"]);
        let m = MockChecker::with_running(["Busy"]);
        let ctx = ScanContext::new(None, &m);
        let items = provider(MockRunner::new())
            .list_caches(&ctx, &root)
            .unwrap();
        assert_eq!(names(&items), vec!["com.example.Idle"]);
        assert_eq!(items[0].evidence, vec![Evidence::OwningAppUnknown]);
        cleanup(&root);
    }

    #[test]
    fn failed_tools_fall_back_to_the_guess_without_failing_the_category() {
        let root = fixture("fail", &["com.microsoft.VSCode"]);
        let query = query(&["com.microsoft", "com.microsoft.VSCode"]);
        let timeout = crate::runner::CommandError::Timeout {
            bin: MDFIND.into(),
            timeout_ms: 1,
        };
        let runner = MockRunner::new().with_output(
            MDFIND,
            &["-attr", "kMDItemCFBundleIdentifier", &query],
            Err(timeout),
        );
        let m = MockChecker::none();
        let ctx = ScanContext::new(None, &m);
        let caches = provider(runner);
        let items = caches.list_caches(&ctx, &root).unwrap();
        assert_eq!(items[0].evidence, vec![Evidence::OwningAppUnknown]);
        let path = root.join("com.microsoft.VSCode");
        assert_eq!(caches.item_apps(&path), vec!["VSCode"]);
        cleanup(&root);
    }

    #[test]
    fn a_failed_plutil_read_is_unresolved() {
        let query = query(&["com.microsoft", "com.microsoft.VSCode"]);
        let runner = MockRunner::new().with_exit(
            MDFIND,
            &["-attr", "kMDItemCFBundleIdentifier", &query],
            0,
            "/Applications/Visual Studio Code.app   kMDItemCFBundleIdentifier = com.microsoft.VSCode\n",
        );
        let caches = provider(runner);
        let path = Path::new("/c/com.microsoft.VSCode");
        assert_eq!(caches.item_apps(path), vec!["VSCode"]);
    }

    #[test]
    fn item_apps_names_the_executable_when_resolved_else_the_guess() {
        let caches = provider(vscode_runner());
        assert_eq!(
            caches.item_apps(Path::new("/c/com.microsoft.VSCode")),
            vec!["Code"]
        );
        assert_eq!(caches.item_apps(Path::new("/c/Homebrew")), vec!["Homebrew"]);
        assert_eq!(
            caches.item_apps(Path::new("/c/com.apple.Safari")),
            vec!["Safari"]
        );
    }

    #[test]
    fn pips_folder_is_left_to_pip_cache_only_when_a_real_pip3_empties_it() {
        let root = fixture("tool-owned", &["pip", "Homebrew"]);
        let found = MockRunner::new().with_which("/opt/homebrew/bin/pip3");
        let caches = UserCaches::with_runner(Arc::new(found));
        let items = caches
            .list_caches(&ScanContext::unchecked(), &root)
            .unwrap();
        assert_eq!(names(&items), vec!["Homebrew"], "pip-cache owns it");
        // pyenv, conda or python.org: no real pip3, so it stays a plain cache.
        let shim_only = MockRunner::new()
            .with_which("pip3")
            .with_which("/usr/bin/pip3");
        let caches = UserCaches::with_runner(Arc::new(shim_only));
        let mut listed = names(
            &caches
                .list_caches(&ScanContext::unchecked(), &root)
                .unwrap(),
        );
        listed.sort();
        assert_eq!(listed, vec!["Homebrew", "pip"]);
    }

    #[test]
    fn discovery_resolves_all_ids_with_one_query_and_spawns_nothing_more_for_item_apps() {
        let root = fixture("spawns", &["com.example.A", "com.example.B", "Homebrew"]);
        let runner = Arc::new(MockRunner::new());
        let caches = UserCaches::with_runner(runner.clone());
        let m = MockChecker::none();
        let ctx = ScanContext::new(None, &m);
        let items = caches.list_caches(&ctx, &root).unwrap();
        for item in &items {
            caches.item_apps(&item.path);
        }
        let calls = runner.output_calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 1, "{calls:?}");
        assert!(calls[0].contains("== 'com.example.A'"));
        assert!(calls[0].contains("== 'com.example.B'"));
        assert!(!calls[0].contains("Homebrew"));
        cleanup(&root);
    }

    fn shipit_runner() -> MockRunner {
        vscode_runner_for(&["com.microsoft.VSCode", "com.microsoft.VSCode.ShipIt"])
    }

    #[test]
    fn a_helper_folder_is_gated_by_its_parent_app_and_the_guess() {
        let root = fixture("shipit-run", &["com.microsoft.VSCode.ShipIt"]);
        let running = MockChecker::with_running(["Code"]);
        let ctx = ScanContext::new(None, &running);
        let items = provider(shipit_runner()).list_caches(&ctx, &root).unwrap();
        assert!(items.is_empty(), "Code is running");
        let running = MockChecker::with_running(["ShipIt"]);
        let ctx = ScanContext::new(None, &running);
        let items = provider(shipit_runner()).list_caches(&ctx, &root).unwrap();
        assert!(items.is_empty(), "the guess is checked too");
        cleanup(&root);
    }

    #[test]
    fn an_idle_helper_folder_is_offered_with_the_parent_executable() {
        let root = fixture("shipit-idle", &["com.microsoft.VSCode.ShipIt"]);
        let m = MockChecker::none();
        let ctx = ScanContext::new(None, &m);
        let caches = provider(shipit_runner());
        let items = caches.list_caches(&ctx, &root).unwrap();
        assert_eq!(
            items[0].evidence,
            vec![Evidence::OwningApp {
                name: "Code".into()
            }]
        );
        assert_eq!(caches.item_apps(&items[0].path), vec!["Code", "ShipIt"]);
        cleanup(&root);
    }

    #[test]
    fn a_folder_that_resolves_itself_ignores_its_parent() {
        let caches = provider(vscode_runner_for(&[
            "com.microsoft",
            "com.microsoft.VSCode",
        ]));
        let path = Path::new("/c/com.microsoft.VSCode");
        assert_eq!(caches.item_apps(path), vec!["Code"]);
    }

    #[test]
    fn long_id_lists_are_split_into_bounded_queries() {
        let runner = Arc::new(MockRunner::new());
        let caches = UserCaches::with_runner(runner.clone());
        let owned: Vec<String> = (0..120).map(|i| format!("com.example.App{i}")).collect();
        let ids: Vec<&str> = owned.iter().map(String::as_str).collect();
        caches.find_apps(&ids);
        let calls = runner.output_calls.lock().unwrap().clone();
        assert_eq!(calls.len(), 3, "{calls:?}");
        for call in &calls {
            assert!(call.matches(" == '").count() <= QUERY_CHUNK);
        }
    }

    #[test]
    fn only_bundle_id_shaped_names_reach_the_query() {
        assert!(is_bundle_id("com.microsoft.VSCode"));
        assert!(is_bundle_id("org.gonhanh.GoNhanh"));
        for bad in ["Homebrew", "a'b.c", "x.y z", "a.b') || ('1", "ünï.code", ""] {
            assert!(!is_bundle_id(bad), "{bad}");
        }
    }

    #[test]
    fn a_failed_probe_keeps_the_path_for_execution_to_recheck() {
        use crate::clean::process::test_support::FailingProbe;
        let root = fixture("probe", &["com.example.Safari"]);
        let ctx = ScanContext::new(None, &FailingProbe);
        let items = provider(MockRunner::new())
            .list_caches(&ctx, &root)
            .unwrap();
        assert_eq!(items.len(), 1);
        cleanup(&root);
    }
}
