//! Running one planned cask action and telling how far it got.

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use super::brew::Brew;
use super::failure::{classify_exit, CaskFailure};
use super::plan::{CaskAction, CaskPlanItem};
use crate::runner::StreamEnd;

/// The longest one install or upgrade may run, downloads included. This is the
/// only hard stop once a cask has begun to install: the runner interrupts
/// `brew` (SIGINT, then SIGKILL after a grace period), as it does for a cancel
/// during a download.
pub const RUN_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// How often a raised cancel flag is looked at while `brew` runs.
const WATCH_INTERVAL: Duration = Duration::from_millis(20);

/// How far a run is, read from `brew`'s `==> ` headings. Never a percentage:
/// `brew` does not print one when it has no terminal. A run only moves
/// forward (the order below): lines from the two pipes can arrive out of
/// order, and a download heading after an install one must not take the
/// label back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Stage {
    /// Before any heading `brew` prints is recognised.
    Working,
    Downloading,
    Installing,
}

impl Stage {
    /// The stable word the client maps to its own copy.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Downloading => "downloading",
            Self::Installing => "installing",
        }
    }
}

/// What a heading says about the run. Homebrew 7 prints the download heading
/// (`==> Fetching downloads for: x`) on stderr when it has no terminal and the
/// install ones (`==> Installing Cask x`, `==> Moving App ...`) on stdout;
/// older versions print `==> Downloading <url>`. Other headings, and
/// `==> Upgrading ...`, change nothing.
fn stage_of(line: &str) -> Option<Stage> {
    let title = line.trim_start().strip_prefix("==>")?.trim_start();
    let starts = |words: &[&str]| words.iter().any(|word| title.starts_with(word));
    if starts(&["Fetching", "Downloading"]) {
        Some(Stage::Downloading)
    } else if starts(&[
        "Installing",
        "Moving",
        "Linking",
        "Backing",
        "Removing",
        "Running installer",
        "Adopting",
        "Quitting",
        "Uninstalling",
        "Purging",
    ]) {
        Some(Stage::Installing)
    } else {
        None
    }
}

/// Where a run is: which cask, how far into it, and which of a batch it is.
/// The token comes with every step so a client shows one app at a time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaskProgress {
    pub token: String,
    pub stage: Stage,
    /// Zero-based position of this cask in the run.
    pub position: u64,
    pub total: u64,
}

/// Receives each change of [`CaskProgress`] on the thread that runs `brew`.
pub type ProgressSink<'a> = Option<&'a dyn Fn(CaskProgress)>;

/// Raises its flag when dropped, whether the run ended or unwound.
struct Finished<'a>(&'a AtomicBool);

impl Drop for Finished<'_> {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}

/// What `brew` says on a line that changes how a run ended although it exits
/// 0. Read while the lines stream, so the tail the runner keeps does not
/// matter. A line counts only when it is about this run's cask (and, for the
/// two warnings, starts with `Warning:` as `brew`'s do), so a caveat or a
/// line about another cask that happens to say the same words does not turn a
/// run that worked into a failure.
struct Said<'a> {
    token: &'a str,
    already_installed: bool,
    not_upgrading: bool,
    manual_installer: bool,
}

impl<'a> Said<'a> {
    fn new(token: &'a str) -> Self {
        Self {
            token,
            already_installed: false,
            not_upgrading: false,
            manual_installer: false,
        }
    }

    fn note(&mut self, line: &str) {
        let line = line.trim().to_lowercase();
        if !names(&line, self.token) {
            return;
        }
        let warning = line.starts_with("warning:");
        self.already_installed |= warning && line.contains("is already installed");
        self.not_upgrading |= warning && line.contains("not upgrading");
        self.manual_installer |= line.contains("only provides a manual installer");
    }
}

/// Whether `line` (lower case) names `token` as a whole word: the characters
/// a token is made of do not continue it on either side.
fn names(line: &str, token: &str) -> bool {
    let token = token.to_lowercase();
    let part = |c: char| c.is_ascii_alphanumeric() || "-_.@+".contains(c);
    line.match_indices(&token).any(|(start, found)| {
        let before = line[..start].chars().next_back();
        let after = line[start + found.len()..].chars().next();
        !before.is_some_and(part) && !after.is_some_and(part)
    })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunOutcome {
    Done,
    Cancelled,
    /// An earlier cask in the run was cancelled, so this one never started.
    NotAttempted,
    Failed(CaskFailure),
}

/// Runs `item` through `brew`, reporting each change of stage (the first is
/// [`Stage::Working`], before `brew` starts). Nothing is spawned for an item
/// that must be refused, or once `cancel` is raised.
///
/// **Cancel.** Raising `cancel` while the cask downloads interrupts `brew`,
/// which has changed nothing yet. Once `brew` reports that it is installing
/// (moving, backing up, removing, linking...) the cancel is deferred for this
/// cask: an interrupt there can leave the old app backed up or removed and the
/// new one half in place, so the cask is let finish and the result is its own
/// ([`RunOutcome::Done`] or a failure). The caller's flag stays raised, so
/// nothing after this cask starts. The only hard stop then is [`RUN_TIMEOUT`].
pub fn run_item(
    brew: &Brew<'_>,
    item: &CaskPlanItem,
    cancel: Option<&AtomicBool>,
    on_stage: &mut dyn FnMut(Stage),
) -> RunOutcome {
    if let Some(failure) = item.refusal() {
        return RunOutcome::Failed(failure);
    }
    let mut stage = Stage::Working;
    on_stage(stage);
    if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
        return RunOutcome::Cancelled;
    }
    let mut said = Said::new(&item.token);
    // What the runner is told: the caller's flag, but only until installing.
    let installing = AtomicBool::new(false);
    let stop = AtomicBool::new(false);
    let finished = AtomicBool::new(false);
    let mut on_line = |line: &str| {
        said.note(line);
        if let Some(next) = stage_of(line).filter(|next| *next > stage) {
            stage = next;
            installing.store(next >= Stage::Installing, Ordering::Release);
            on_stage(next);
        }
    };
    let ended = thread::scope(|scope| {
        // Ends the watcher even if a callback panics: the scope would
        // otherwise wait for it for ever while the panic unwinds.
        let _watch_over = Finished(&finished);
        if let Some(cancel) = cancel {
            scope.spawn(|| {
                while !finished.load(Ordering::Acquire) {
                    if cancel.load(Ordering::Acquire) && !installing.load(Ordering::Acquire) {
                        stop.store(true, Ordering::Release);
                    }
                    thread::sleep(WATCH_INTERVAL);
                }
            });
        }
        let ended = brew.stream(
            &item.args(),
            RUN_TIMEOUT,
            cancel.map(|_| &stop),
            &mut on_line,
        );
        ended
    });
    match ended {
        Err(failure) => RunOutcome::Failed(failure),
        Ok(StreamEnd::Cancelled) => RunOutcome::Cancelled,
        Ok(StreamEnd::Finished(outcome)) if !outcome.success() => {
            RunOutcome::Failed(classify_exit(&item.token, &outcome))
        }
        // `brew` exits 0 when it finds nothing to do, and says so.
        Ok(StreamEnd::Finished(_)) => {
            let token = item.token.clone();
            match item.action {
                _ if said.manual_installer => {
                    RunOutcome::Failed(CaskFailure::ManualInstaller { token })
                }
                CaskAction::Install if said.already_installed => {
                    RunOutcome::Failed(CaskFailure::AlreadyInstalled { token })
                }
                CaskAction::Update if said.not_upgrading => {
                    RunOutcome::Failed(CaskFailure::NotOutdated { token })
                }
                _ => RunOutcome::Done,
            }
        }
    }
}

#[cfg(test)]
mod process_tests;
#[cfg(test)]
mod tests;
