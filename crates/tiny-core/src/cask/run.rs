//! Running one planned cask action and telling how far it got.

use std::sync::atomic::AtomicBool;
use std::time::Duration;

use super::brew::Brew;
use super::failure::{classify_exit, CaskFailure};
use super::plan::{CaskAction, CaskPlanItem};
use crate::runner::StreamEnd;

/// The longest one install or upgrade may run, downloads included.
pub const RUN_TIMEOUT: Duration = Duration::from_secs(30 * 60);

/// How far a run is, read from `brew`'s `==> ` headings. Never a percentage:
/// `brew` does not print one when it has no terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

/// What `brew` says on a line that changes how a run ended although it exits
/// 0. Read while the lines stream, so the tail the runner keeps does not
/// matter.
#[derive(Default)]
struct Said {
    already_installed: bool,
    not_upgrading: bool,
    manual_installer: bool,
}

impl Said {
    fn note(&mut self, line: &str) {
        let line = line.to_lowercase();
        self.already_installed |= line.contains("is already installed");
        self.not_upgrading |= line.contains("not upgrading");
        self.manual_installer |= line.contains("only provides a manual installer");
    }
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
/// that must be refused, or once `cancel` is raised. Raising `cancel` while
/// `brew` runs interrupts it so it can undo what it began.
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
    let mut said = Said::default();
    let mut on_line = |line: &str| {
        said.note(line);
        if let Some(next) = stage_of(line).filter(|next| *next != stage) {
            stage = next;
            on_stage(next);
        }
    };
    match brew.stream(&item.args(), RUN_TIMEOUT, cancel, &mut on_line) {
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
mod tests;
