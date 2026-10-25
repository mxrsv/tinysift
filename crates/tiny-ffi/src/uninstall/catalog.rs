//! Where the installed-app inventory comes from. Production reads
//! `/Applications` through core (`defaults`, `mdls`); tests inject fixtures so
//! they never depend on what is installed or on macOS tools.

use std::path::Path;

use tiny_core::cask::list_apps_in;
use tiny_core::uninstall::{
    list_app_identities, list_applications, read_bundle_executable, AppEntry, APPLICATIONS_DIR,
};

use crate::clean::app_runner;
use crate::FfiError;

pub(crate) trait AppCatalog {
    /// The folder an app bundle must sit directly inside.
    fn applications_root(&self) -> &Path;
    /// Every installed app with size and last use.
    fn list(&self) -> Result<Vec<AppEntry>, FfiError>;
    /// Every installed app with only its name, path and bundle identifier;
    /// the cheap list the duplicate-bundle-id check (O5) needs.
    fn identities(&self) -> Result<Vec<AppEntry>, FfiError>;
    /// The app's process name (`CFBundleExecutable`), if it can be read.
    fn executable(&self, app: &Path) -> Option<String>;
    /// The apps directly inside `~/Applications`, for the cask code, which asks
    /// whether an app is on the Mac wherever the user keeps it. Uninstall never
    /// calls it and keeps listing `/Applications` alone, so a fake inventory
    /// need not have any.
    fn user_identities(&self, _home: &Path) -> Vec<AppEntry> {
        Vec::new()
    }
}

/// The real inventory.
pub(crate) struct SystemCatalog;

impl AppCatalog for SystemCatalog {
    fn applications_root(&self) -> &Path {
        Path::new(APPLICATIONS_DIR)
    }

    fn list(&self) -> Result<Vec<AppEntry>, FfiError> {
        Ok(list_applications()?)
    }

    fn identities(&self) -> Result<Vec<AppEntry>, FfiError> {
        Ok(list_app_identities()?)
    }

    fn executable(&self, app: &Path) -> Option<String> {
        read_bundle_executable(app)
    }

    fn user_identities(&self, home: &Path) -> Vec<AppEntry> {
        list_apps_in(&home.join("Applications"), app_runner().as_ref())
    }
}
