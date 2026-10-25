//! One cask a preview shows and a run acts on.

use std::fs;
use std::path::PathBuf;

use super::elevation::Elevation;
use super::failure::CaskFailure;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaskAction {
    Install,
    Update,
}

/// What `brew` will do to one cask, as read when the preview was built.
/// Execution acts on this and never on anything the caller sends back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaskPlanItem {
    pub action: CaskAction,
    pub token: String,
    pub name: String,
    /// The version on the Mac now; `None` for an install.
    pub from_version: Option<String>,
    /// The version the run installs.
    pub version: String,
    pub download_host: Option<String>,
    /// Why the run is refused, known from `brew info` when the preview was
    /// built: the installer needs an administrator password, or there is
    /// nothing for `brew` to run.
    pub blocked: Option<CaskFailure>,
    /// Where the cask places its apps: shown to the user, checked for a
    /// collision before an install and for a running copy before an update.
    pub apps: Vec<PathBuf>,
}

/// What stops `action` on a cask whose artifacts ask this of `sudo`. An
/// update counts the installer too: it installs the new copy.
pub(super) fn blocked_by(
    elevation: Elevation,
    action: CaskAction,
    token: &str,
) -> Option<CaskFailure> {
    let needs_password = match action {
        CaskAction::Install => elevation.install,
        CaskAction::Update => elevation.update,
    };
    if elevation.manual_installer {
        Some(CaskFailure::ManualInstaller {
            token: token.to_string(),
        })
    } else {
        needs_password.then_some(CaskFailure::NeedsAdministrator)
    }
}

impl CaskPlanItem {
    /// The arguments `brew` is given, after its own path.
    pub fn args(&self) -> Vec<&str> {
        match self.action {
            CaskAction::Install => vec!["install", "--cask", &self.token],
            // A running-app probe cannot cover an app opened while brew fetches.
            // Older brew versions must reject this flag, never retry without it.
            CaskAction::Update => vec!["upgrade", "--cask", "--no-quit", &self.token],
        }
    }

    /// The command as the review sheet shows it.
    pub fn command(&self) -> String {
        format!("brew {}", self.args().join(" "))
    }

    /// Why this item must not be run at all, checked right before it is.
    pub fn refusal(&self) -> Option<CaskFailure> {
        self.blocked.clone().or_else(|| self.app_in_the_way())
    }

    /// For an install, something already where the cask would put its app.
    pub(super) fn app_in_the_way(&self) -> Option<CaskFailure> {
        if self.action != CaskAction::Install {
            return None;
        }
        let taken = self
            .apps
            .iter()
            .find(|app| fs::symlink_metadata(app).is_ok())?;
        Some(CaskFailure::AppExists {
            path: taken.to_string_lossy().into_owned(),
        })
    }
}
