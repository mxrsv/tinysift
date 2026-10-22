//! Tool caches in `~/.cache`, each offered only where the tool's own layout
//! shows what is safe to remove.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use crate::error::Result;

use super::{execute_per_item, root_as_item, CleanProvider};
use crate::clean::fs_safe::{is_dir_safe, list_children};
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};

const CACHE_DIR: &str = ".cache";

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn cache_dir(home: &Path, tool: &str) -> PathBuf {
    home.join(CACHE_DIR).join(tool)
}

fn file_name(path: &Path) -> Option<&str> {
    path.file_name().and_then(|name| name.to_str())
}

// ---------- Puppeteer ----------

const PUPPETEER_ID: &str = "puppeteer";
const PUPPETEER_LABEL: &str = "Puppeteer browsers";
const PUPPETEER_DIR: &str = "puppeteer";

/// Browser folders Puppeteer downloads by default, with the process name
/// each runs as (Chrome for Testing's `CFBundleExecutable`). Anything else
/// in the cache is left out.
const PUPPETEER_BROWSERS: &[(&str, &str)] = &[
    ("chrome", "Google Chrome for Testing"),
    ("chrome-headless-shell", "chrome-headless-shell"),
];

/// A downloaded build is `<platform>-<build id>`.
const MAC_PLATFORMS: &[&str] = &["mac", "mac_arm"];

pub struct Puppeteer;

impl CleanProvider for Puppeteer {
    fn id(&self) -> &'static str {
        PUPPETEER_ID
    }
    fn label(&self) -> &'static str {
        PUPPETEER_LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Browsers Puppeteer downloaded for testing; npx puppeteer browsers install fetches them again"
            .into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Safe
    }
    fn desktop_trash_paths(&self) -> bool {
        true
    }
    fn available(&self) -> bool {
        home().is_some_and(|h| is_dir_safe(&cache_dir(&h, PUPPETEER_DIR)))
    }
    fn item_apps(&self, path: &Path) -> Vec<String> {
        let browser = path.parent().and_then(file_name);
        match PUPPETEER_BROWSERS
            .iter()
            .find(|(dir, _)| Some(*dir) == browser)
        {
            Some((_, app)) => vec![app.to_string()],
            // An unrecognised path is gated on every browser rather than none.
            None => PUPPETEER_BROWSERS
                .iter()
                .map(|(_, app)| app.to_string())
                .collect(),
        }
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        Ok(home()
            .map(|h| puppeteer_builds(ctx, &h))
            .unwrap_or_default())
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_per_item(items, action, PUPPETEER_ID)
    }
    fn check_item(&self, path: &Path) -> std::result::Result<(), String> {
        match home() {
            Some(h) if is_puppeteer_build(&h, path) => Ok(()),
            _ => Err("not a browser Puppeteer downloaded".into()),
        }
    }
}

/// Downloaded builds of the known browsers, except one whose browser runs.
fn puppeteer_builds(ctx: &ScanContext<'_>, home: &Path) -> Vec<CleanItem> {
    let root = cache_dir(home, PUPPETEER_DIR);
    let mut items = Vec::new();
    for (browser, app) in PUPPETEER_BROWSERS {
        if ctx.known_running(app) {
            continue;
        }
        for path in list_children(&root.join(browser), ctx) {
            if is_puppeteer_build(home, &path) {
                items.extend(root_as_item(
                    ctx,
                    &path,
                    PUPPETEER_ID,
                    PUPPETEER_LABEL,
                    RiskLevel::Safe,
                ));
            }
        }
    }
    items
}

/// `~/.cache/puppeteer/<known browser>/<mac platform>-<build>`, a real folder.
fn is_puppeteer_build(home: &Path, path: &Path) -> bool {
    let Some(browser) = path.parent() else {
        return false;
    };
    let known_browser =
        file_name(browser).is_some_and(|b| PUPPETEER_BROWSERS.iter().any(|(dir, _)| *dir == b));
    let build = file_name(path)
        .and_then(|name| name.split_once('-'))
        .is_some_and(|(platform, id)| MAC_PLATFORMS.contains(&platform) && !id.is_empty());
    browser.parent() == Some(cache_dir(home, PUPPETEER_DIR).as_path())
        && known_browser
        && build
        && is_dir_safe(path)
}

// ---------- Hugging Face ----------

const HUGGING_FACE_ID: &str = "huggingface";
const HUGGING_FACE_LABEL: &str = "Hugging Face models";
/// The download cache. Its `models--*` folders only link into the shared
/// `hub/blobs`, so the whole folder is the unit that frees space. Tokens
/// sit beside it and are never offered.
const HUGGING_FACE_HUB: &str = "huggingface/hub";

pub struct HuggingFace;

impl CleanProvider for HuggingFace {
    fn id(&self) -> &'static str {
        HUGGING_FACE_ID
    }
    fn label(&self) -> &'static str {
        HUGGING_FACE_LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Models and datasets Hugging Face downloaded; they download again on next use, \
         which can be large"
            .into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    fn desktop_trash_paths(&self) -> bool {
        true
    }
    fn available(&self) -> bool {
        home().is_some_and(|h| is_dir_safe(&cache_dir(&h, HUGGING_FACE_HUB)))
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        Ok(home()
            .map(|h| hugging_face_hub(ctx, &h))
            .unwrap_or_default())
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_per_item(items, action, HUGGING_FACE_ID)
    }
    fn check_item(&self, path: &Path) -> std::result::Result<(), String> {
        match home() {
            Some(h) if path == cache_dir(&h, HUGGING_FACE_HUB) => Ok(()),
            _ => Err("not the Hugging Face download cache".into()),
        }
    }
}

fn hugging_face_hub(ctx: &ScanContext<'_>, home: &Path) -> Vec<CleanItem> {
    let hub = cache_dir(home, HUGGING_FACE_HUB);
    if !is_dir_safe(&hub) {
        return Vec::new();
    }
    root_as_item(
        ctx,
        &hub,
        HUGGING_FACE_ID,
        HUGGING_FACE_LABEL,
        RiskLevel::Review,
    )
}

// ---------- Codex runtimes ----------

const CODEX_RUNTIMES_ID: &str = "codex-runtimes";
const CODEX_RUNTIMES_LABEL: &str = "Codex runtime install leftovers";
const CODEX_RUNTIMES_DIR: &str = "codex-runtimes";

/// The ChatGPT app installs the Codex runtime: it downloads and unpacks into
/// a `mkdtemp` folder with this prefix, renames the result to
/// `codex-primary-runtime` and removes the folder when the install ends. A
/// folder left behind is from an install that died. The active runtime is
/// never offered.
const STAGING_PREFIX: &str = "codex-runtime-install-";
const RUNTIME_INSTALLER: &str = "ChatGPT";
/// Older than any install still running.
const STAGING_MIN_AGE: Duration = Duration::from_secs(24 * 60 * 60);

pub struct CodexRuntimes;

impl CleanProvider for CodexRuntimes {
    fn id(&self) -> &'static str {
        CODEX_RUNTIMES_ID
    }
    fn label(&self) -> &'static str {
        CODEX_RUNTIMES_LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Folders an interrupted Codex runtime install left behind; the runtime in use is never \
         listed"
            .into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    fn requires_app_quit(&self) -> Option<&'static str> {
        Some(RUNTIME_INSTALLER)
    }
    fn desktop_trash_paths(&self) -> bool {
        true
    }
    fn available(&self) -> bool {
        home().is_some_and(|h| is_dir_safe(&cache_dir(&h, CODEX_RUNTIMES_DIR)))
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        Ok(home()
            .map(|h| install_leftovers(ctx, &h, STAGING_MIN_AGE))
            .unwrap_or_default())
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_per_item(items, action, CODEX_RUNTIMES_ID)
    }
    fn check_item(&self, path: &Path) -> std::result::Result<(), String> {
        match home() {
            Some(h) if is_install_leftover(&h, path) => Ok(()),
            _ => Err("not a Codex runtime install folder".into()),
        }
    }
}

fn install_leftovers(ctx: &ScanContext<'_>, home: &Path, min_age: Duration) -> Vec<CleanItem> {
    let mut items = Vec::new();
    for path in list_children(&cache_dir(home, CODEX_RUNTIMES_DIR), ctx) {
        if is_install_leftover(home, &path) && older_than(&path, min_age) {
            items.extend(root_as_item(
                ctx,
                &path,
                CODEX_RUNTIMES_ID,
                CODEX_RUNTIMES_LABEL,
                RiskLevel::Review,
            ));
        }
    }
    items
}

fn is_install_leftover(home: &Path, path: &Path) -> bool {
    path.parent() == Some(cache_dir(home, CODEX_RUNTIMES_DIR).as_path())
        && file_name(path).is_some_and(|name| {
            name.strip_prefix(STAGING_PREFIX)
                .is_some_and(|rest| !rest.is_empty())
        })
        && is_dir_safe(path)
}

/// An unreadable or future time counts as recent.
fn older_than(path: &Path, min_age: Duration) -> bool {
    std::fs::symlink_metadata(path)
        .and_then(|meta| meta.modified())
        .ok()
        .and_then(|time| SystemTime::now().duration_since(time).ok())
        .is_some_and(|age| age >= min_age)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::process::test_support::MockChecker;
    use std::fs;

    fn paths(items: &[CleanItem]) -> Vec<PathBuf> {
        items.iter().map(|i| i.path.clone()).collect()
    }

    fn dir(path: PathBuf) -> PathBuf {
        fs::create_dir_all(&path).unwrap();
        fs::write(path.join("file"), b"payload").unwrap();
        path
    }

    #[test]
    fn puppeteer_offers_only_downloaded_builds_of_known_browsers() {
        let home = tempfile::tempdir().unwrap();
        let root = cache_dir(home.path(), PUPPETEER_DIR);
        let chrome = dir(root.join("chrome/mac_arm-153.0.8010.36"));
        let shell = dir(root.join("chrome-headless-shell/mac-153.0.8010.36"));
        dir(root.join("firefox/mac_arm-140.0"));
        dir(root.join("chrome/linux-153.0.8010.36"));
        dir(root.join("chrome/notes"));
        dir(root.join("other"));
        fs::write(root.join("chrome/.metadata"), b"{}").unwrap();
        std::os::unix::fs::symlink(&chrome, root.join("chrome/mac_arm-1")).unwrap();
        let items = puppeteer_builds(&ScanContext::unchecked(), home.path());
        assert_eq!(paths(&items), vec![chrome.clone(), shell.clone()]);
        assert!(items.iter().all(|i| i.risk == RiskLevel::Safe));
        assert!(is_puppeteer_build(home.path(), &chrome));
        assert!(!is_puppeteer_build(home.path(), &root.join("chrome")));
        assert!(!is_puppeteer_build(
            home.path(),
            &root.join("chrome/mac_arm-1")
        ));
        assert!(!is_puppeteer_build(home.path(), &root));
        let outside = dir(home.path().join("other/chrome/mac_arm-153.0.8010.36"));
        assert!(!is_puppeteer_build(home.path(), &outside));
    }

    #[test]
    fn puppeteer_skips_a_browser_while_it_runs() {
        let home = tempfile::tempdir().unwrap();
        let root = cache_dir(home.path(), PUPPETEER_DIR);
        let chrome = dir(root.join("chrome/mac_arm-153.0.8010.36"));
        let shell = dir(root.join("chrome-headless-shell/mac_arm-153.0.8010.36"));
        let checker = MockChecker::with_running(["chrome-headless-shell"]);
        let items = puppeteer_builds(&ScanContext::new(None, &checker), home.path());
        assert_eq!(paths(&items), vec![chrome.clone()]);
        assert_eq!(Puppeteer.item_apps(&chrome), ["Google Chrome for Testing"]);
        assert_eq!(Puppeteer.item_apps(&shell), ["chrome-headless-shell"]);
        assert_eq!(Puppeteer.item_apps(Path::new("/x/y")).len(), 2);
    }

    #[test]
    fn hugging_face_offers_the_hub_folder_alone() {
        let home = tempfile::tempdir().unwrap();
        let root = cache_dir(home.path(), "huggingface");
        let hub = dir(root.join("hub/blobs/78"));
        let hub = hub.parent().unwrap().parent().unwrap().to_path_buf();
        dir(root.join("hub/models--org--model/snapshots"));
        fs::write(root.join("token"), b"hf_secret").unwrap();
        dir(root.join("stored_tokens"));
        dir(root.join("xet/logs"));
        let items = hugging_face_hub(&ScanContext::unchecked(), home.path());
        assert_eq!(paths(&items), vec![hub]);
        assert_eq!(items[0].risk, RiskLevel::Review);
        assert!(HuggingFace.quit_apps().is_empty());
    }

    #[test]
    fn codex_runtimes_offers_only_old_install_leftovers() {
        let home = tempfile::tempdir().unwrap();
        let root = cache_dir(home.path(), CODEX_RUNTIMES_DIR);
        let leftover = dir(root.join("codex-runtime-install-HegXc5/payload"));
        let leftover = leftover.parent().unwrap().to_path_buf();
        dir(root.join("codex-primary-runtime/dependencies"));
        dir(root.join("codex-primary-runtime.previous"));
        dir(root.join("codex-runtime-install-"));
        fs::write(root.join("codex-runtime-install-file"), b"x").unwrap();
        std::os::unix::fs::symlink(&leftover, root.join("codex-runtime-install-link")).unwrap();
        let items = install_leftovers(&ScanContext::unchecked(), home.path(), Duration::ZERO);
        assert_eq!(paths(&items), vec![leftover.clone()]);
        // One made just now may belong to an install that is still running.
        let recent = install_leftovers(&ScanContext::unchecked(), home.path(), STAGING_MIN_AGE);
        assert!(recent.is_empty());
        assert!(!is_install_leftover(
            home.path(),
            &root.join("codex-primary-runtime")
        ));
        assert_eq!(CodexRuntimes.quit_apps(), ["ChatGPT"]);
    }

    #[test]
    fn guards_refuse_paths_outside_each_pinned_shape() {
        // HOME is the real one here; none of these paths has the pinned shape.
        for path in ["/", "/tmp/chrome/mac_arm-1", "/tmp/codex-runtime-install-x"] {
            let path = Path::new(path);
            assert!(Puppeteer.check_item(path).is_err());
            assert!(HuggingFace.check_item(path).is_err());
            assert!(CodexRuntimes.check_item(path).is_err());
        }
        let h = home().unwrap();
        assert!(HuggingFace
            .check_item(&cache_dir(&h, "huggingface/token"))
            .is_err());
        assert!(CodexRuntimes
            .check_item(&cache_dir(&h, "codex-runtimes/codex-primary-runtime"))
            .is_err());
    }
}
