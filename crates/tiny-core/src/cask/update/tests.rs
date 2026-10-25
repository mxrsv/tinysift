use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use super::*;
use crate::cask::test_support::*;
use crate::cask::Stage;
use crate::clean::process::test_support::{FailingProbe, MockChecker};
use crate::runner::test_support::MockRunner;

fn brew(runner: &MockRunner) -> Brew<'_> {
    Brew::new(Path::new(BREW_PATH), runner)
}

/// The process name of `X.app` in these tests is `XExec`.
fn executable(path: &Path) -> Option<String> {
    Some(format!("{}Exec", path.file_stem()?.to_str()?))
}

fn installed_app(dir: &Path, name: &str) -> PathBuf {
    let app = dir.join(format!("{name}.app"));
    std::fs::create_dir(&app).unwrap();
    app
}

fn listed(token: &str) -> OutdatedCask {
    OutdatedCask {
        token: token.into(),
        name: token.into(),
        from_version: "1.0".into(),
        version: "2.0".into(),
        download_host: None,
        blocked: None,
        apps: Vec::new(),
    }
}

mod plan;
mod run;
mod state;
