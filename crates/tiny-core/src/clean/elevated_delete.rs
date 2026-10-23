//! Deletion as administrator, for the system simulator caches that root owns
//! (`needs_admin`). One call deletes a whole batch after one macOS
//! authentication dialog: `/usr/bin/osascript` runs a constant script whose
//! `do shell script … with administrator privileges` starts Apple's
//! `/bin/sh`, which runs only `/usr/bin/pgrep` and `/bin/rm` as root. Nothing
//! is installed and no credential is kept. The entries reach the root shell
//! as bare names in argv, each through `quoted form of`; no path is ever
//! part of the script text.

use std::path::Path;

use super::providers::system_simulator_caches::check_system_cache_path;

/// Deletes paths for good as root. A seam: only `clean_remove` builds the
/// real `OsascriptDelete`, so no test can show the dialog or delete real data.
pub trait ElevatedDelete: Send + Sync {
    /// Deletes every path in `paths` after one authentication dialog. After
    /// `Failed`, part of them may be gone: callers decide each path's outcome
    /// from what is left on disk.
    fn delete(&self, paths: &[&Path]) -> Result<(), ElevatedError>;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ElevatedError {
    /// The user cancelled the authentication dialog: nothing ran as root.
    Cancelled,
    /// A process that uses the caches was running, found as root
    /// (`pgrep -x` name): nothing was deleted.
    Busy(String),
    /// Refused before the dialog, or failed after it; part may be gone.
    Failed(String),
}

/// The real route: `APPLESCRIPT` through `/usr/bin/osascript`, only for
/// entries directly inside `CACHES_ROOT` (`check_system_cache_path`).
pub struct OsascriptDelete;

impl ElevatedDelete for OsascriptDelete {
    fn delete(&self, paths: &[&Path]) -> Result<(), ElevatedError> {
        let names = entry_names(paths, check_system_cache_path).map_err(ElevatedError::Failed)?;
        run_script(APPLESCRIPT, &names)
    }
}

/// The bare name of each path, the only thing the root shell receives,
/// once `check` accepts the path: absolute, UTF-8 without NUL, outside
/// `/System`, directly inside the allowed folder and not a symlink. One
/// refusal refuses the whole batch, before any dialog.
fn entry_names<'a>(
    paths: &[&'a Path],
    check: impl Fn(&Path) -> Result<(), String>,
) -> Result<Vec<&'a str>, String> {
    paths
        .iter()
        .map(|path| {
            check(path).map_err(|reason| format!("{}: {reason}", path.display()))?;
            path.file_name()
                .and_then(|name| name.to_str())
                .filter(|name| !matches!(*name, "" | "." | "..") && !name.contains(['/', '\0']))
                .ok_or_else(|| format!("{}: not a bare entry name", path.display()))
        })
        .collect()
}

/// Run by `osascript` with the entry names as argv. The root shell gets them
/// as `$1…`, each through `quoted form of`. It refuses while any of
/// `SIMULATOR_ACTIVITY` runs (exit 75, the name on stderr), enters the fixed
/// caches folder physically and checks that it is exactly that folder, then
/// deletes each bare, non-symlink name without crossing into another volume.
/// The answer is `0`, or the error number and its message on separate lines;
/// -128 is a cancelled dialog.
const APPLESCRIPT: &str = r#"on run argv
	set guardStep to "for app in Xcode Simulator xcodebuild xctest XCTRunner simctl launchd_sim; do /usr/bin/pgrep -xq \"$app\"; case $? in 0) echo \"$app\" >&2; exit 75;; 1) ;; *) echo 'could not check for running simulator tools' >&2; exit 70;; esac; done; "
	set enterStep to "root=/Library/Developer/CoreSimulator/Caches; cd -P \"$root\" && [ \"$(pwd -P)\" = \"$root\" ] || { echo \"$root is not a real folder\" >&2; exit 70; }; "
	set removeStep to "status=0; for name in \"$@\"; do case $name in ''|.|..|*/*) echo \"refused: $name\" >&2; status=1; continue;; esac; if [ -L \"./$name\" ]; then echo \"refused symlink: $name\" >&2; status=1; continue; fi; /bin/rm -rfx -- \"./$name\" || status=1; done; exit $status"
	set command to "/bin/sh -c " & quoted form of (guardStep & enterStep & removeStep) & " tinysift"
	repeat with entry in argv
		set command to command & space & quoted form of (contents of entry)
	end repeat
	try
		do shell script command with prompt "TinySift wants to delete the system simulator caches." with administrator privileges without altering line endings
		return "0"
	on error message number code
		return (code as text) & linefeed & message
	end try
end run"#;

#[cfg(target_os = "macos")]
const OSASCRIPT: &str = "/usr/bin/osascript";

/// AppleScript's error number for a cancelled dialog.
#[cfg(target_os = "macos")]
const USER_CANCELED: i64 = -128;

/// The root shell's exit status while a simulator tool runs (EX_TEMPFAIL).
#[cfg(target_os = "macos")]
const BUSY: i64 = 75;

/// Runs `script` through `osascript` with `names` as argv, in a clean
/// environment so nothing from this process reaches the root shell. No
/// timeout: the dialog waits for the user, and the caller trusts only what
/// is left on disk afterwards.
#[cfg(target_os = "macos")]
fn run_script(script: &str, names: &[&str]) -> Result<(), ElevatedError> {
    use std::process::{Command, Stdio};
    if names.is_empty() {
        return Ok(());
    }
    let output = Command::new(OSASCRIPT)
        .env_clear()
        .env("PATH", "/usr/bin:/bin:/usr/sbin:/sbin")
        .args(["-e", script])
        // After `--`, a name starting with `-` is never an osascript option.
        .arg("--")
        .args(names)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| ElevatedError::Failed(format!("{OSASCRIPT}: {e}")))?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(ElevatedError::Failed(stderr.trim().to_string()));
    }
    answer(&String::from_utf8_lossy(&output.stdout))
}

#[cfg(not(target_os = "macos"))]
fn run_script(_: &str, _: &[&str]) -> Result<(), ElevatedError> {
    Err(ElevatedError::Failed(
        "deletion as administrator needs macOS".into(),
    ))
}

/// The script's answer: `0`, or an error number and message on two lines.
#[cfg(target_os = "macos")]
fn answer(stdout: &str) -> Result<(), ElevatedError> {
    let (code, message) = stdout.split_once('\n').unwrap_or((stdout, ""));
    let message = message.trim().to_string();
    match code.trim().parse::<i64>() {
        Ok(0) => Ok(()),
        Ok(USER_CANCELED) => Err(ElevatedError::Cancelled),
        Ok(BUSY) => Err(ElevatedError::Busy(message)),
        Ok(code) => Err(ElevatedError::Failed(format!("{message} ({code})"))),
        Err(_) => Err(ElevatedError::Failed(format!(
            "unexpected osascript answer: {}",
            stdout.trim()
        ))),
    }
}

#[cfg(test)]
mod tests;
