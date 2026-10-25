//! Installing one catalogue app: the preview's plan, then the run.

use std::sync::atomic::AtomicBool;

use super::brew::Brew;
use super::catalogue::{catalogue_entry, installed_copy};
use super::failure::CaskFailure;
use super::info::{display_version, read_cask_info};
use super::plan::{blocked_by, CaskAction, CaskPlanItem};
use super::run::{run_item, CaskProgress, ProgressSink, RunOutcome};
use crate::uninstall::AppEntry;

/// What installing `token` would do, read from `brew` without changing
/// anything. `installed` is the Mac's app listing, read again for this
/// preview: an app with the catalogue entry's bundle identifier stops the plan
/// whatever it is called or where it sits, because `brew` would install a
/// second copy beside a moved one. A token outside the catalogue is refused
/// before a process spawns. A cask that needs an administrator password, or
/// only has a manual installer, still gets a plan with `blocked` set, so the
/// sheet can say why it cannot be installed; the run refuses it. Something
/// already in the way of the cask's app, or a cask Homebrew lists, is refused
/// here.
pub fn plan_install(
    brew: &Brew<'_>,
    token: &str,
    installed: &[AppEntry],
) -> Result<CaskPlanItem, CaskFailure> {
    let entry = catalogue_entry(token).ok_or_else(|| CaskFailure::NotInCatalogue {
        token: token.to_string(),
    })?;
    if let Some(app) = installed_copy(entry, installed) {
        return Err(CaskFailure::AppExists {
            path: app.path.to_string_lossy().into_owned(),
        });
    }
    let info = read_cask_info(brew, &[entry.token])?
        .into_iter()
        .find(|info| info.token == entry.token)
        .ok_or_else(|| CaskFailure::CaskNotFound {
            token: entry.token.to_string(),
        })?;
    if info.installed.is_some() {
        return Err(CaskFailure::AlreadyInstalled { token: info.token });
    }
    let item = CaskPlanItem {
        action: CaskAction::Install,
        blocked: blocked_by(info.elevation, CaskAction::Install, &info.token),
        token: info.token,
        name: entry.name.to_string(),
        from_version: None,
        version: display_version(&info.version).to_string(),
        download_host: info.download_host,
        apps: info.apps,
    };
    match item.app_in_the_way() {
        Some(failure) => Err(failure),
        None => Ok(item),
    }
}

/// Runs an install plan, reporting each stage as progress.
pub fn run_install(
    brew: &Brew<'_>,
    item: &CaskPlanItem,
    cancel: Option<&AtomicBool>,
    progress: ProgressSink<'_>,
) -> RunOutcome {
    run_item(brew, item, cancel, &mut |stage| {
        if let Some(progress) = progress {
            progress(CaskProgress {
                token: item.token.clone(),
                stage,
                position: 0,
                total: 1,
            });
        }
    })
}

#[cfg(test)]
mod tests;
