//! What Scan reads and the one thing it does (show a file in Finder), behind
//! a trait so tests neither walk the disk nor open Finder.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

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
        run_quietly(reveal_command(path))
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

/// Runs `command` away from the TUI's terminal: no input, its output
/// dropped, its error output captured and returned as the reason it failed,
/// so nothing it prints lands on the screen (C3).
pub fn run_quietly(mut command: Command) -> Result<(), String> {
    let output = command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .output()
        .map_err(|error| format!("open could not start: {error}"))?;
    if output.status.success() {
        return Ok(());
    }
    let said = String::from_utf8_lossy(&output.stderr);
    let said = said.split_whitespace().collect::<Vec<_>>().join(" ");
    Err(if said.is_empty() {
        format!("open ended with {}", output.status)
    } else {
        said
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    fn shell(script: &str) -> Command {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", script]);
        command
    }

    #[test]
    fn a_failure_returns_what_the_command_said_and_nothing_reaches_the_screen() {
        let failed = run_quietly(shell(
            "echo noise; echo 'The file /x does not exist.' >&2; exit 1",
        ));
        assert_eq!(failed, Err("The file /x does not exist.".into()));
        let silent = run_quietly(shell("exit 3"));
        assert!(silent.unwrap_err().contains("exit status: 3"));
        assert_eq!(run_quietly(shell("echo fine")), Ok(()));
    }

    #[test]
    fn the_command_gets_no_input_from_the_terminal() {
        // `cat` returns at once on an empty input instead of waiting for keys.
        let started = std::time::Instant::now();
        assert_eq!(run_quietly(Command::new("/bin/cat")), Ok(()));
        assert!(started.elapsed() < std::time::Duration::from_secs(5));
    }

    #[test]
    fn show_in_finder_passes_the_path_as_one_argument_without_a_shell() {
        let path = Path::new("/Users/someone/Downloads/a file; rm -rf ~ $(x).iso");
        let command = reveal_command(path);
        assert_eq!(command.get_program(), OsStr::new("/usr/bin/open"));
        let args: Vec<&OsStr> = command.get_args().collect();
        assert_eq!(args, [OsStr::new("-R"), path.as_os_str()]);
    }
}
