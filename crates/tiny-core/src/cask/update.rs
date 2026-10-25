//! Updating the apps `brew` tracks, one cask at a time.
//!
//! **Open apps.** TinySift never quits an app, and an open one is not
//! replaced. `brew upgrade` quits a running app itself, so every upgrade is
//! given `--no-quit` as a backstop, and the app is probed three times: before
//! a run is accepted ([`check_apps_closed`], so the client can tell the user to
//! quit it), when a cask's turn comes, and once more after the two `brew`
//! reads that follow (each takes about half a second) and right before the
//! upgrade spawns. The probe is `pgrep -x` on the app's process name
//! (`CFBundleExecutable`) through an [`AppProbe`]; a name that cannot be read
//! or a probe that fails counts as "cannot tell" and refuses the update, never
//! as "not running". An app opened after the last probe cannot be seen;
//! `--no-quit` keeps `brew` from closing it.
//!
//! **Only real updates.** A named `brew upgrade --cask <token>` always acts
//! greedily, so before each one the default `brew outdated` is read again and
//! a token that has dropped out of it (the app updated itself since the
//! preview) is not upgraded. That list is not enough either: for a cask that
//! does not declare `auto_updates`, `brew` compares its own record with the
//! cask, so an app that updated itself past the cask is still listed. The
//! app's version is therefore read again too and an update towards a version
//! the app is already at or past is refused ([`version`](super::version)).

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;

use super::brew::Brew;
use super::failure::CaskFailure;
use super::info::{display_version, read_cask_info, valid_token};
use super::outdated::{still_outdated, OutdatedCask};
use super::plan::{blocked_by, CaskAction, CaskPlanItem};
use super::run::{run_item, CaskProgress, ProgressSink, RunOutcome};
use super::version::offers_update;
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

/// Why an update request could not be planned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateRequestError {
    /// The request is wrong: a token that is not a bare cask token (a flag,
    /// a path), that is not in the update list, or that is repeated.
    Invalid(String),
    /// A token from the list that cannot be updated (`NotOutdated`: it has
    /// left `brew outdated` or the app already is its version), or `brew`
    /// itself failed.
    Failed(CaskFailure),
}

impl From<CaskFailure> for UpdateRequestError {
    fn from(failure: CaskFailure) -> Self {
        Self::Failed(failure)
    }
}

/// What updating `tokens` would do, read again from `brew` so a listing that
/// has gone stale cannot be acted on. Every token must be in `listing` (the
/// casks the last `brew outdated` named) and none may repeat, else the request
/// is [`UpdateRequestError::Invalid`] and nothing is asked of `brew`. A token
/// that is no longer outdated, or whose app is already at or past the cask's
/// version, is `Failed(NotOutdated)`. A cask that needs an administrator
/// password is planned with `blocked` set, and the run refuses it.
pub fn plan_updates(
    brew: &Brew<'_>,
    listing: &[OutdatedCask],
    tokens: &[String],
) -> Result<Vec<CaskPlanItem>, UpdateRequestError> {
    let invalid = |token: &str, why: &str| UpdateRequestError::Invalid(format!("{token:?} {why}"));
    for (index, token) in tokens.iter().enumerate() {
        if !valid_token(token) {
            return Err(invalid(token, "is not a cask token"));
        }
        if tokens[..index].contains(token) {
            return Err(invalid(token, "is listed twice"));
        }
        if !listing.iter().any(|cask| &cask.token == token) {
            return Err(invalid(token, "is not in the update list"));
        }
    }
    let wanted: Vec<&str> = tokens.iter().map(String::as_str).collect();
    let infos = read_cask_info(brew, &wanted)?;
    tokens
        .iter()
        .map(|token| {
            let not_outdated = || {
                UpdateRequestError::Failed(CaskFailure::NotOutdated {
                    token: token.clone(),
                })
            };
            let info = infos
                .iter()
                .find(|info| &info.token == token && info.outdated)
                .filter(|info| {
                    // Not an update when the app already is that version, or
                    // newer: see `version`.
                    info.from_version()
                        .is_none_or(|current| offers_update(current, &info.version))
                })
                .ok_or_else(not_outdated)?;
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
        .find_map(|item| open_app(item, probe, executable))
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
    let fail = RunOutcome::Failed;
    let not_outdated = || {
        fail(CaskFailure::NotOutdated {
            token: item.token.clone(),
        })
    };
    if let Some(failure) = item.refusal() {
        return fail(failure);
    }
    if let Some(failure) = open_app(item, probe, executable) {
        return fail(failure);
    }
    // The default list: a named `upgrade` is greedy, so a cask that has left
    // it must not reach `brew upgrade`.
    match still_outdated(brew, &item.token) {
        Err(failure) => return fail(failure),
        Ok(false) => return not_outdated(),
        Ok(true) => {}
    }
    // Fresh versions: the list stays true for a cask that does not declare
    // `auto_updates` even once the app has gone past it, and then an upgrade
    // would be a downgrade.
    match read_cask_info(brew, &[&item.token]) {
        Err(failure) => return fail(failure),
        Ok(infos) => {
            let past = infos
                .iter()
                .find(|info| info.token == item.token)
                .is_some_and(|info| {
                    info.from_version()
                        .is_some_and(|current| !offers_update(current, &info.version))
                });
            if past {
                return not_outdated();
            }
        }
    }
    // Last, because both reads above took time: an app opened since the first
    // probe would be quit by `brew` (it is also told `--no-quit`).
    match open_app(item, probe, executable) {
        Some(failure) => fail(failure),
        None => run_item(brew, item, cancel, on_stage),
    }
}

/// Why `item` must not be upgraded because of its apps: open, or not
/// possible to tell.
fn open_app(
    item: &CaskPlanItem,
    probe: &dyn AppProbe,
    executable: ExecutableOf<'_>,
) -> Option<CaskFailure> {
    match run_state(&item.apps, probe, executable) {
        RunState::NotRunning => None,
        RunState::Running { app } => Some(CaskFailure::AppRunning { app }),
        RunState::Unknown { app, error } => Some(CaskFailure::AppCheckFailed { app, error }),
    }
}

#[cfg(test)]
mod tests;
