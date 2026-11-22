//! Whether any process of a browser runs, which refuses a privacy write
//! or Undo. A browser runs when Clean's name check ([`AppProbe`], `pgrep
//! -x` on [`BrowserKind::app_names`]) sees it, or when any process runs
//! from an app bundle the browser owns ([`BrowserKind::owns_bundle_id`]:
//! the main app or a helper). An answer that is not known is an error, and
//! callers refuse on it.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::time::Duration;

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind};

use crate::browsers::BrowserKind;
use crate::clean::process::AppProbe;
use crate::runner::CommandRunner;

const PLUTIL: &str = "/usr/bin/plutil";
const PLUTIL_TIMEOUT: Duration = Duration::from_secs(5);
const INFO_PLIST: &str = "Contents/Info.plist";
const BUNDLE_ID_KEY: &str = "CFBundleIdentifier";

/// The running check a privacy write and its Undo ask.
pub trait BrowserProcesses {
    /// `Ok(true)` when a process of `kind` runs; `Err` when unknown.
    fn running(&self, kind: BrowserKind) -> Result<bool, String>;
}

/// The bundle ids of the apps whose processes run now.
pub trait RunningBundles {
    fn bundle_ids(&self) -> Result<Vec<String>, String>;
}

/// Clean's name probe plus the running bundles.
pub struct RunningCheck<'a> {
    pub names: &'a dyn AppProbe,
    pub bundles: &'a dyn RunningBundles,
}

impl BrowserProcesses for RunningCheck<'_> {
    fn running(&self, kind: BrowserKind) -> Result<bool, String> {
        for name in kind.app_names() {
            if self.names.probe(name)? {
                return Ok(true);
            }
        }
        let ids = self.bundles.bundle_ids()?;
        Ok(ids.iter().any(|id| kind.owns_bundle_id(id)))
    }
}

/// Reads every process's executable path and the bundle id of the
/// outermost `.app` that holds it. A browser's helpers live inside its
/// app (`Google Chrome.app/Contents/Frameworks/…/Google Chrome Helper.app`,
/// `Firefox.app/Contents/MacOS/plugin-container.app`), so the outer bundle
/// is the browser's own. A process whose path cannot be read (another
/// user's) is skipped; a bundle with no id names no browser.
pub struct LiveBundles<'r> {
    pub runner: &'r dyn CommandRunner,
}

impl RunningBundles for LiveBundles<'_> {
    fn bundle_ids(&self) -> Result<Vec<String>, String> {
        let mut system = System::new();
        system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::new().with_exe(UpdateKind::Always),
        );
        let bundles: BTreeSet<PathBuf> = system
            .processes()
            .values()
            .filter_map(|process| outermost_app(process.exe()?))
            .collect();
        let mut ids = Vec::new();
        for bundle in bundles {
            if let Some(id) = self.bundle_id(&bundle)? {
                ids.push(id);
            }
        }
        Ok(ids)
    }
}

impl LiveBundles<'_> {
    /// `CFBundleIdentifier` from the bundle's `Info.plist`, read by
    /// `plutil` (which reads binary and XML plists alike).
    fn bundle_id(&self, bundle: &Path) -> Result<Option<String>, String> {
        let plist = bundle.join(INFO_PLIST);
        // Absolute, so `plutil` never reads it as an option.
        let Some(plist) = plist.to_str().filter(|p| p.starts_with('/')) else {
            return Ok(None);
        };
        let outcome = self
            .runner
            .output(
                PLUTIL,
                &["-extract", BUNDLE_ID_KEY, "raw", "-o", "-", plist],
                PLUTIL_TIMEOUT,
            )
            .map_err(|e| format!("could not read {plist}: {e}"))?;
        let id = outcome.stdout.trim();
        Ok((outcome.status == Some(0) && !id.is_empty()).then(|| id.to_string()))
    }
}

/// The outermost folder named `*.app` on the path, if any.
fn outermost_app(exe: &Path) -> Option<PathBuf> {
    exe.ancestors()
        .filter(|p| {
            p.extension()
                .is_some_and(|ext| ext.eq_ignore_ascii_case("app"))
        })
        .last()
        .map(Path::to_path_buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::process::test_support::{FailingProbe, MockChecker};

    pub(crate) struct FakeBundles(pub Result<Vec<String>, String>);

    impl RunningBundles for FakeBundles {
        fn bundle_ids(&self) -> Result<Vec<String>, String> {
            self.0.clone()
        }
    }

    fn check(names: &dyn AppProbe, bundles: &FakeBundles) -> Result<bool, String> {
        RunningCheck { names, bundles }.running(BrowserKind::Chrome)
    }

    #[test]
    fn the_app_name_a_helper_or_an_unknown_answer_counts() {
        let none = MockChecker::none();
        let quiet = FakeBundles(Ok(vec!["com.apple.Mail".into()]));
        assert_eq!(check(&none, &quiet), Ok(false));
        assert_eq!(
            check(&MockChecker::with_running(["Google Chrome"]), &quiet),
            Ok(true)
        );
        let helper = FakeBundles(Ok(vec!["com.google.Chrome.helper.renderer".into()]));
        assert_eq!(check(&none, &helper), Ok(true));
        assert!(check(&FailingProbe, &quiet).is_err());
        assert!(check(&none, &FakeBundles(Err("no list".into()))).is_err());
    }

    /// Firefox's `firefox` process, or any edition's bundle, holds it.
    #[test]
    fn every_firefox_edition_and_its_process_name_count() {
        let firefox = |names: &dyn AppProbe, bundles: &FakeBundles| {
            RunningCheck { names, bundles }.running(BrowserKind::Firefox)
        };
        let none = MockChecker::none();
        let quiet = FakeBundles(Ok(Vec::new()));
        assert_eq!(firefox(&none, &quiet), Ok(false));
        assert_eq!(
            firefox(&MockChecker::with_running(["firefox"]), &quiet),
            Ok(true)
        );
        for id in ["org.mozilla.firefoxdeveloperedition", "org.mozilla.nightly"] {
            assert_eq!(
                firefox(&none, &FakeBundles(Ok(vec![id.into()]))),
                Ok(true),
                "{id}"
            );
        }
    }

    #[test]
    fn the_outermost_app_holds_the_helpers() {
        let exe = Path::new(
            "/Applications/Google Chrome.app/Contents/Frameworks/F.framework/Helpers/H.app/Contents/MacOS/H",
        );
        assert_eq!(
            outermost_app(exe),
            Some(PathBuf::from("/Applications/Google Chrome.app"))
        );
        assert_eq!(outermost_app(Path::new("/usr/bin/sleep")), None);
    }

    /// A real process inside a made-up bundle with a Chrome helper's id.
    #[test]
    fn a_live_process_in_an_owned_bundle_is_seen() {
        use crate::runner::RealRunner;
        let dir = tempfile::tempdir().unwrap();
        let bundle = dir.path().join("Fixture Helper.app");
        std::fs::create_dir_all(bundle.join("Contents")).unwrap();
        std::fs::write(
            bundle.join(INFO_PLIST),
            "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict>\
             <key>CFBundleIdentifier</key><string>com.google.Chrome.helper.fixture</string>\
             </dict></plist>\n",
        )
        .unwrap();
        let mut child = crate::clean::process::test_support::run_from(
            &bundle.join("Contents/MacOS"),
            "tiny-fixture-browser-helper",
        );
        let ids = LiveBundles {
            runner: &RealRunner,
        }
        .bundle_ids();
        let _ = child.kill();
        let _ = child.wait();
        let ids = ids.unwrap();
        assert!(
            ids.iter()
                .any(|id| id == "com.google.Chrome.helper.fixture"),
            "{ids:?}"
        );
    }
}
