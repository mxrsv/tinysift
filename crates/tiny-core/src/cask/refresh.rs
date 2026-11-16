//! Refreshing Homebrew's index (`brew update`), the one write the app makes
//! to Homebrew without the user pressing a button.
//!
//! Every other `brew` call runs with `HOMEBREW_NO_AUTO_UPDATE`, so the update
//! list is as old as the last `brew update`. The app runs this at most once a
//! day in the background. It updates Homebrew itself and its taps and may run
//! Homebrew's migrations; it installs and upgrades nothing.
//!
//! A refresh never fails loudly: contention is [`IndexRefresh::Deferred`] (try
//! again later) and anything that ran to an end without refreshing is
//! [`IndexRefresh::Ended`]. Only a missing Homebrew is an error. There is no
//! cancel; [`REFRESH_TIMEOUT`] is the one hard stop, for a `brew` that hangs.

use std::time::Duration;

use super::brew::Brew;
use super::failure::{classify_exit, CaskFailure};

/// A refresh fetches over the network; this only stops one that hangs. A kill
/// mid-fetch can leave a git lock behind in a tap, so it is generous.
pub const REFRESH_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// What `brew update` says when another `brew` holds a lock it needs, in
/// Homebrew 7's own words: its `update` lock ("Another `brew update` process
/// is already running", `utils/lock.sh`) or an API file another `brew` is
/// downloading ("Cannot lock <file> for download!", `cmd/update.sh`). Never
/// git's "cannot lock ref": a stale ref lock lasts, and as a deferral it would
/// run `brew update` every hour without ever counting an attempt.
const CONTENTION: &[&str] = &["process is already running", "for download!"];

/// How a refresh ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IndexRefresh {
    /// `brew update` finished: the index is as new as Homebrew's servers.
    Refreshed,
    /// Another `brew` held a lock: nothing ran to an end, try again later.
    Deferred,
    /// It ran and stopped without refreshing everything: no network, a
    /// timeout, or a failure of Homebrew's own. The index is what it was, or
    /// partly refreshed.
    Ended(CaskFailure),
}

/// Runs `brew update`. `Err` only when there is no `brew` to run.
pub fn refresh_index(brew: &Brew<'_>) -> Result<IndexRefresh, CaskFailure> {
    match brew.output(&["update"], REFRESH_TIMEOUT) {
        Err(CaskFailure::HomebrewMissing) => Err(CaskFailure::HomebrewMissing),
        Err(CaskFailure::TimedOut { .. }) => {
            Ok(IndexRefresh::Ended(CaskFailure::TimedOut { running: true }))
        }
        Err(failure) => Ok(IndexRefresh::Ended(failure)),
        Ok(outcome) if outcome.success() => Ok(IndexRefresh::Refreshed),
        Ok(outcome) => {
            let text = format!("{}\n{}", outcome.stderr, outcome.stdout).to_lowercase();
            Ok(if CONTENTION.iter().any(|needle| text.contains(needle)) {
                IndexRefresh::Deferred
            } else {
                IndexRefresh::Ended(classify_exit("update", &outcome))
            })
        }
    }
}

#[cfg(test)]
mod tests;
