//! What Scan reads and the one thing it does (show a file in Finder), behind
//! a trait so tests neither walk the disk nor open Finder.

use std::path::{Path, PathBuf};
use std::process::Command;

use tiny_core::options::ScanOptions;
use tiny_core::scan::types::ScanData;

pub trait ScanEngine: Send + Sync + 'static {
    /// The command's scan with its defaults; `progress` gets each message the
    /// engine reports. Blocks until the walk ends; runs on a worker thread.
    fn scan(&self, progress: &(dyn Fn(String) + Send + Sync)) -> Result<ScanData, String>;
    /// Selects `path` in a Finder window.
    fn reveal(&self, path: &Path) -> Result<(), String>;
    /// Paths under it are shown as `~/…`.
    fn home(&self) -> Option<PathBuf>;
}

/// `tiny_core::scan::scan`, the call `sift scan` makes, with the same defaults
/// (100 MB, 90 days, Downloads, Desktop and Documents, `.tinyignore` honoured).
pub struct RealScan;

impl ScanEngine for RealScan {
    fn scan(&self, progress: &(dyn Fn(String) + Send + Sync)) -> Result<ScanData, String> {
        let report = |event: tiny_core::progress::Progress| progress(event.message);
        tiny_core::scan::scan(&ScanOptions::default(), Some(&report)).map_err(|e| e.to_string())
    }

    fn reveal(&self, path: &Path) -> Result<(), String> {
        let status = reveal_command(path)
            .status()
            .map_err(|error| format!("open could not start: {error}"))?;
        if status.success() {
            Ok(())
        } else {
            Err(format!("open ended with {status}"))
        }
    }

    fn home(&self) -> Option<PathBuf> {
        std::env::var_os("HOME").map(PathBuf::from)
    }
}

/// `open -R <path>` as a child process: the path is one argument, never
/// parsed by a shell.
pub fn reveal_command(path: &Path) -> Command {
    let mut command = Command::new("/usr/bin/open");
    command.arg("-R").arg(path);
    command
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    #[test]
    fn show_in_finder_passes_the_path_as_one_argument_without_a_shell() {
        let path = Path::new("/Users/someone/Downloads/a file; rm -rf ~ $(x).iso");
        let command = reveal_command(path);
        assert_eq!(command.get_program(), OsStr::new("/usr/bin/open"));
        let args: Vec<&OsStr> = command.get_args().collect();
        assert_eq!(args, [OsStr::new("-R"), path.as_os_str()]);
    }
}
