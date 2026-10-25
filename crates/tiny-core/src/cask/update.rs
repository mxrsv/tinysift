//! Updating the apps `brew` tracks, one cask at a time.
//!
//! An app that is open is never replaced, and TinySift never quits it:
//! `brew upgrade` quits a running app itself, so the run must not reach
//! `brew` while one is open. The check is the one Uninstall makes: the app's
//! process name (`CFBundleExecutable`) matched with `pgrep -x` through an
//! [`AppProbe`]. A name that cannot be read or a probe that fails counts as
//! "cannot tell" and refuses the update, never as "not running". It is made
//! twice: [`check_apps_closed`] before a run is accepted, so the client can
//! tell the user to quit the app, and again right before each `brew` starts,
//! because an app can be opened during a long batch.
//!
//! A named `brew upgrade --cask <token>` always acts greedily. So before each
//! one the default `brew outdated` is read again and a token that has dropped
//! out of it (the app updated itself since the preview) is not upgraded.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use super::brew::Brew;
use super::failure::CaskFailure;
use super::info::{display_version, read_cask_info, valid_token};
use super::outdated::{still_outdated, OutdatedCask};
use super::plan::{blocked_by, CaskAction, CaskPlanItem};
use super::run::{run_item, CaskProgress, ProgressSink, RunOutcome};
use crate::clean::process::AppProbe;

/// Whether the apps of a cask are open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunState {
    NotRunning,
    Running {
        app: String,
    },
    /// The process name could not be read or the check failed.
    Unknown {
        app: String,
        error: String,
    },
}

/// Reads the process name of an installed app (`read_bundle_executable` in
/// the app; a fixture in tests).
pub type ExecutableOf<'a> = &'a dyn Fn(&Path) -> Option<String>;

/// Whether any of `apps` that is installed is open. An open app wins over a
/// check that failed, which wins over "not running". A path with nothing at
/// it has nothing to be running.
pub fn run_state(apps: &[PathBuf], probe: &dyn AppProbe, executable: ExecutableOf<'_>) -> RunState {
    let mut unknown = None;
    for path in apps
        .iter()
        .filter(|path| fs::symlink_metadata(path).is_ok())
    {
        let app = path
            .file_stem()
            .map_or_else(|| path.to_string_lossy(), |stem| stem.to_string_lossy())
            .into_owned();
        let Some(process) = executable(path) else {
            unknown.get_or_insert(RunState::Unknown {
                app,
                error: "the app's process name could not be read".into(),
            });
            continue;
        };
        match probe.probe(&process) {
            Ok(true) => return RunState::Running { app },
            Ok(false) => {}
            Err(error) => {
                unknown.get_or_insert(RunState::Unknown { app, error });
            }
        }
    }
    unknown.unwrap_or(RunState::NotRunning)
}

/// What updating `tokens` would do, read again from `brew` so a listing that
/// has gone stale cannot be acted on. Every token must be in `listing` (the
/// casks the last `brew outdated` named) and still outdated; none may repeat.
/// A cask that needs an administrator password is planned with `blocked`
/// set, and the run refuses it.
pub fn plan_updates(
    brew: &Brew<'_>,
    listing: &[OutdatedCask],
    tokens: &[String],
) -> Result<Vec<CaskPlanItem>, CaskFailure> {
    let not_outdated = |token: &str| CaskFailure::NotOutdated {
        token: token.to_string(),
    };
    if let Some(bad) = tokens.iter().find(|token| !valid_token(token)) {
        return Err(not_outdated(bad));
    }
    for (index, token) in tokens.iter().enumerate() {
        if tokens[..index].contains(token) || !listing.iter().any(|cask| &cask.token == token) {
            return Err(not_outdated(token));
        }
    }
    let wanted: Vec<&str> = tokens.iter().map(String::as_str).collect();
    let infos = read_cask_info(brew, &wanted)?;
    tokens
        .iter()
        .map(|token| {
            let info = infos
                .iter()
                .find(|info| &info.token == token && info.outdated)
                .ok_or_else(|| not_outdated(token))?;
            Ok(CaskPlanItem {
                action: CaskAction::Update,
                token: info.token.clone(),
                name: info.name.clone(),
                from_version: info.from_version().map(|v| display_version(v).to_string()),
                version: display_version(&info.version).to_string(),
                download_host: info.download_host.clone(),
                blocked: blocked_by(info.elevation, CaskAction::Update, &info.token),
                apps: info.apps.clone(),
            })
        })
        .collect()
}

/// The first of `items` that is open (`AppRunning`) or whose state cannot be
/// told (`AppCheckFailed`), else `Ok`. Nothing is spawned.
pub fn check_apps_closed(
    items: &[CaskPlanItem],
    probe: &dyn AppProbe,
    executable: ExecutableOf<'_>,
) -> Result<(), CaskFailure> {
    items
        .iter()
        .find_map(|item| match run_state(&item.apps, probe, executable) {
            RunState::NotRunning => None,
            RunState::Running { app } => Some(CaskFailure::AppRunning { app }),
            RunState::Unknown { app, error } => Some(CaskFailure::AppCheckFailed { app, error }),
        })
        .map_or(Ok(()), Err)
}

/// Updates `items` in order, one `brew upgrade --cask <token>` at a time.
/// Right before each, it is refused without spawning an upgrade when it is
/// blocked, when its apps are open or cannot be checked, or when it has left
/// `brew outdated`; the run goes on with the next. A failure never stops the
/// others; cancelling does, and what was not started is
/// [`RunOutcome::NotAttempted`]. Each stage is reported with the token and the
/// cask's position in the run.
pub fn run_updates(
    brew: &Brew<'_>,
    items: &[CaskPlanItem],
    probe: &dyn AppProbe,
    executable: ExecutableOf<'_>,
    cancel: Option<&AtomicBool>,
    progress: ProgressSink<'_>,
) -> Vec<(String, RunOutcome)> {
    let total = items.len() as u64;
    let mut cancelled = false;
    items
        .iter()
        .enumerate()
        .map(|(index, item)| {
            let outcome = if cancelled {
                RunOutcome::NotAttempted
            } else {
                run_one(brew, item, probe, executable, cancel, &mut |stage| {
                    if let Some(progress) = progress {
                        progress(CaskProgress {
                            token: item.token.clone(),
                            stage,
                            position: index as u64,
                            total,
                        });
                    }
                })
            };
            cancelled |= outcome == RunOutcome::Cancelled;
            (item.token.clone(), outcome)
        })
        .collect()
}

fn run_one(
    brew: &Brew<'_>,
    item: &CaskPlanItem,
    probe: &dyn AppProbe,
    executable: ExecutableOf<'_>,
    cancel: Option<&AtomicBool>,
    on_stage: &mut dyn FnMut(super::run::Stage),
) -> RunOutcome {
    if let Some(failure) = item.refusal() {
        return RunOutcome::Failed(failure);
    }
    match run_state(&item.apps, probe, executable) {
        RunState::Running { app } => return RunOutcome::Failed(CaskFailure::AppRunning { app }),
        RunState::Unknown { app, error } => {
            return RunOutcome::Failed(CaskFailure::AppCheckFailed { app, error })
        }
        RunState::NotRunning => {}
    }
    match still_outdated(brew, &item.token) {
        Err(failure) => RunOutcome::Failed(failure),
        Ok(false) => RunOutcome::Failed(CaskFailure::NotOutdated {
            token: item.token.clone(),
        }),
        Ok(true) => run_item(brew, item, cancel, on_stage),
    }
}

#[cfg(test)]
mod tests;
