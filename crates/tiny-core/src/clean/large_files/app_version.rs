//! The version of an installed app, read from its bundle's `Info.plist`.

use std::path::Path;

use crate::runner::CommandRunner;

/// Absolute path: a Finder-launched app has a minimal `PATH`.
const PLUTIL: &str = "/usr/bin/plutil";

/// Where an app's version comes from; tests answer from a table.
pub trait AppVersions {
    /// `CFBundleShortVersionString` of the bundle at `app`, if it has one.
    fn version(&self, app: &Path) -> Option<String>;
}

/// Reads the version with `plutil`, through the bounded runner.
pub struct PlutilVersions<'r>(pub &'r dyn CommandRunner);

impl AppVersions for PlutilVersions<'_> {
    fn version(&self, app: &Path) -> Option<String> {
        let plist = app.join("Contents/Info.plist");
        let out = self.0.run(
            PLUTIL,
            &[
                "-extract",
                "CFBundleShortVersionString",
                "raw",
                "-o",
                "-",
                &plist.to_string_lossy(),
            ],
        );
        let version = out.stdout.trim();
        (out.success && !version.is_empty()).then(|| version.to_string())
    }
}
