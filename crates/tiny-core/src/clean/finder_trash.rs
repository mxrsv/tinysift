//! Move to Trash through Finder, so Finder's Put Back works. From the app
//! this needs Automation (Apple Events) permission for Finder; a denial is
//! its own error and never falls back to deletion.

use std::path::Path;
use std::time::Duration;

use crate::runner::{CommandRunner, RealRunner};

/// `as alias` resolves a final symlink, so `refuse_symlink` runs first.
const FINDER_DELETE_SCRIPT: &str = "on run argv\n\
     tell application \"Finder\" to delete (POSIX file (item 1 of argv) as alias)\n\
     end run";

/// Without `with timeout`, AppleScript gives up on Finder after about two
/// minutes (error -1712) while Finder keeps emptying, which would read as a
/// failure with the space freed. Shorter than `TRASH_TIMEOUT`, so osascript
/// answers before the runner kills it.
const EMPTY_EVENT_TIMEOUT_SECS: u64 = 9 * 60;
const _: () = assert!(EMPTY_EVENT_TIMEOUT_SECS < TRASH_TIMEOUT.as_secs());

fn empty_script() -> String {
    format!(
        "with timeout of {EMPTY_EVENT_TIMEOUT_SECS} seconds\n\
         \ttell application \"Finder\" to empty trash\n\
         end timeout"
    )
}

/// Absolute so a Finder-launched app does not depend on its minimal `PATH`.
const OSASCRIPT: &str = "/usr/bin/osascript";

/// Generous: the first call blocks on the Automation prompt until the user
/// answers, and Finder may take long to move a large directory.
const TRASH_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Apple Events error for "not authorized to send Apple events".
const AUTOMATION_DENIED: i32 = -1743;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TrashError {
    #[error("automation permission denied: {0}")]
    AutomationDenied(String),
    #[error("move to Trash failed: {0}")]
    Failed(String),
}

pub trait Trash: Send + Sync {
    /// Moves `path` to the user's Trash. On `Err` the source is left as is.
    fn move_to_trash(&self, path: &Path) -> Result<(), TrashError>;

    /// Empties the user's Trash for good. A route that cannot refuses.
    fn empty_trash(&self) -> Result<(), TrashError> {
        Err(TrashError::Failed(
            "this Trash route cannot empty the Trash".into(),
        ))
    }
}

/// The default route: Finder via `osascript`.
pub struct FinderTrash;

impl Trash for FinderTrash {
    fn move_to_trash(&self, path: &Path) -> Result<(), TrashError> {
        refuse_symlink(path)?;
        let posix = path
            .to_str()
            .ok_or_else(|| TrashError::Failed(format!("non-UTF-8 path: {}", path.display())))?;
        let outcome = RealRunner
            .output(
                OSASCRIPT,
                &["-e", FINDER_DELETE_SCRIPT, posix],
                TRASH_TIMEOUT,
            )
            .map_err(|e| TrashError::Failed(e.to_string()))?;
        if outcome.success() {
            return Ok(());
        }
        Err(classify_failure(outcome.stderr.trim()))
    }

    /// Finder's Empty Trash, which also empties the Trash folders of other
    /// volumes and anything put there while it waits for the Automation
    /// prompt; it needs the same Automation permission as a move.
    fn empty_trash(&self) -> Result<(), TrashError> {
        let outcome = RealRunner
            .output(OSASCRIPT, &["-e", &empty_script()], TRASH_TIMEOUT)
            .map_err(|e| TrashError::Failed(e.to_string()))?;
        if outcome.success() {
            return Ok(());
        }
        Err(classify_failure(outcome.stderr.trim()))
    }
}

/// Finder would trash a link's target, never the link itself.
fn refuse_symlink(path: &Path) -> Result<(), TrashError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => Err(TrashError::Failed(format!(
            "refusing to move a symlink: {}",
            path.display()
        ))),
        Ok(_) => Ok(()),
        Err(e) => Err(TrashError::Failed(format!("{}: {e}", path.display()))),
    }
}

/// The AppleScript error number `osascript` ends its message with, as in
/// "execution error: Not authorized to send Apple events to Finder. (-1743)".
fn error_code(stderr: &str) -> Option<i32> {
    let inner = stderr.trim_end().strip_suffix(')')?;
    inner[inner.rfind('(')? + 1..].parse().ok()
}

fn classify_failure(stderr: &str) -> TrashError {
    if error_code(stderr) == Some(AUTOMATION_DENIED) {
        TrashError::AutomationDenied(stderr.to_string())
    } else {
        TrashError::Failed(stderr.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apple_events_denial_is_its_own_error() {
        let denied = "execution error: Not authorized to send Apple events to Finder. (-1743)";
        assert!(matches!(
            classify_failure(denied),
            TrashError::AutomationDenied(_)
        ));
        let missing = "execution error: Finder got an error: Can’t get alias. (-1728)";
        assert!(matches!(classify_failure(missing), TrashError::Failed(_)));
        // A path that merely contains "-1743" is not a denial.
        let in_path =
            "execution error: Can’t get alias \"/Users/me/build-1743 (-1743) copy\". (-1728)";
        assert!(matches!(classify_failure(in_path), TrashError::Failed(_)));
        assert!(matches!(classify_failure("no code"), TrashError::Failed(_)));
    }

    #[test]
    fn emptying_waits_for_finder_longer_than_applescripts_default_and_not_past_the_runner() {
        let script = empty_script();
        assert!(script.contains("with timeout of 540 seconds"), "{script}");
        assert!(script.contains("empty trash") && script.ends_with("end timeout"));
        assert!(Duration::from_secs(EMPTY_EVENT_TIMEOUT_SECS) > Duration::from_secs(120));
    }

    #[test]
    fn a_symlink_never_reaches_finder() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target");
        std::fs::create_dir(&target).unwrap();
        let link = dir.path().join("link");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(
            matches!(refuse_symlink(&link), Err(TrashError::Failed(m)) if m.contains("symlink"))
        );
        assert!(refuse_symlink(&target).is_ok());
        assert!(refuse_symlink(&dir.path().join("missing")).is_err());
    }
}
