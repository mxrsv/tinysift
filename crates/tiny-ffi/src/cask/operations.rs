//! The operations behind the exported methods, over injected seams so tests
//! drive them with a fake `brew`, inventory and probe.

use std::path::{Path, PathBuf};
use std::sync::{MutexGuard, PoisonError};
use std::time::{Instant, SystemTime};

use tiny_core::cask::{
    brew_env_warnings, catalogue_with_presence, check_apps_closed, list_outdated, list_tracked,
    plan_install, plan_updates, run_install, run_state, run_updates, Brew, BrewEnvWarning,
    CaskAction, CaskFailure, CaskPlanItem, CaskProgress, HomebrewStatus, RunOutcome,
};

use super::error::failed;
use super::records;
use super::{
    CaskProgressListener, CaskState, FfiAppRunState, FfiCaskCatalogueApp, FfiCaskError,
    FfiCaskItemResult, FfiCaskPreview, FfiCaskReport, FfiOutdatedCask, Seams, StoredPreview,
};
use crate::clean::PREVIEW_TTL;
use crate::session::CancellationToken;
use crate::TinySession;

/// The `brew` to run, or the failure that says there is none.
fn brew_path(seams: &Seams<'_>) -> Result<PathBuf, FfiCaskError> {
    match seams.brew.status() {
        // Without the account's home `brew` cannot run, which is not the same
        // as there being no Homebrew.
        HomebrewStatus::Present(_) if seams.brew.home().is_none() => Err(FfiCaskError::Operation {
            detail: "this account's home folder could not be read, and Homebrew needs it".into(),
        }),
        HomebrewStatus::Present(path) => Ok(path),
        HomebrewStatus::Missing => Err(failed(CaskFailure::HomebrewMissing)),
    }
}

fn run_state_of(apps: &[PathBuf], seams: &Seams<'_>) -> FfiAppRunState {
    run_state(apps, seams.probe, &|app: &Path| {
        seams.catalog.executable(app)
    })
    .into()
}

fn warnings_for(path: &Path, seams: &Seams<'_>) -> Vec<BrewEnvWarning> {
    brew_env_warnings(path, seams.brew.home().as_deref())
}

impl CaskState {
    fn next_id(&mut self) -> u64 {
        self.next_id += 1;
        self.next_id
    }
}

impl TinySession {
    pub(super) fn cask_state(&self) -> MutexGuard<'_, CaskState> {
        self.cask.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// `cask_catalogue` with injected seams (the test seam).
    pub(crate) fn catalogue(
        &self,
        seams: &Seams<'_>,
    ) -> Result<Vec<FfiCaskCatalogueApp>, FfiCaskError> {
        let _gate = self.begin()?;
        let installed = seams.catalog.identities()?;
        // Best effort: without Homebrew's list the row is just not marked.
        let tracked: Vec<String> = match seams.brew.status() {
            HomebrewStatus::Present(path) => list_tracked(&Brew::new(&path, seams.brew.runner()))
                .map(|casks| casks.into_iter().map(|cask| cask.token).collect())
                .unwrap_or_default(),
            HomebrewStatus::Missing => Vec::new(),
        };
        Ok(catalogue_with_presence(&installed)
            .into_iter()
            .map(|app| {
                let is_tracked = tracked.iter().any(|token| token == app.entry.token);
                records::catalogue_record(app, is_tracked)
            })
            .collect())
    }

    /// `cask_outdated` with injected seams (the test seam).
    pub(crate) fn outdated(&self, seams: &Seams<'_>) -> Result<Vec<FfiOutdatedCask>, FfiCaskError> {
        let _gate = self.begin()?;
        let path = brew_path(seams)?;
        let casks = list_outdated(&Brew::new(&path, seams.brew.runner())).map_err(failed)?;
        let records = casks
            .iter()
            .map(|cask| records::outdated_record(cask, run_state_of(&cask.apps, seams)))
            .collect();
        let mut state = self.cask_state();
        state.listing = Some(casks);
        state.preview = None;
        Ok(records)
    }

    /// `cask_install_preview` with injected seams (the test seam).
    pub(crate) fn install_preview(
        &self,
        token: &str,
        seams: &Seams<'_>,
    ) -> Result<FfiCaskPreview, FfiCaskError> {
        let _gate = self.begin()?;
        let path = brew_path(seams)?;
        // Read again for this preview: the Mac may have changed since the list.
        let installed = seams.catalog.identities()?;
        let item = plan_install(&Brew::new(&path, seams.brew.runner()), token, &installed)
            .map_err(failed)?;
        Ok(self.store_preview(CaskAction::Install, vec![item], &path, seams))
    }

    /// `cask_update_preview` with injected seams (the test seam).
    pub(crate) fn update_preview(
        &self,
        tokens: &[String],
        seams: &Seams<'_>,
    ) -> Result<FfiCaskPreview, FfiCaskError> {
        let _gate = self.begin()?;
        let path = brew_path(seams)?;
        let listing =
            self.cask_state()
                .listing
                .clone()
                .ok_or_else(|| FfiCaskError::PreviewInvalid {
                    detail: "no current update list; look for updates again".into(),
                })?;
        if tokens.is_empty() {
            return Err(FfiCaskError::InvalidInput {
                detail: "nothing to update".into(),
            });
        }
        let items = plan_updates(&Brew::new(&path, seams.brew.runner()), &listing, tokens)?;
        Ok(self.store_preview(CaskAction::Update, items, &path, seams))
    }

    /// Freezes `items` as the session's one preview and describes it. The
    /// open-app state is read before the state lock is taken.
    fn store_preview(
        &self,
        action: CaskAction,
        items: Vec<CaskPlanItem>,
        brew: &Path,
        seams: &Seams<'_>,
    ) -> FfiCaskPreview {
        let records = items
            .iter()
            .map(|item| {
                let run = match action {
                    CaskAction::Install => FfiAppRunState::NotRunning,
                    CaskAction::Update => run_state_of(&item.apps, seams),
                };
                records::preview_item(item, run)
            })
            .collect();
        let warnings = warnings_for(brew, seams).iter().map(Into::into).collect();
        let mut state = self.cask_state();
        let ttl = state.ttl.unwrap_or(PREVIEW_TTL);
        let id = format!("k{}", state.next_id());
        let expires_at_wall = SystemTime::now() + ttl;
        state.preview = Some(StoredPreview {
            id: id.clone(),
            action,
            items,
            expires_at: Instant::now() + ttl,
            expires_at_wall,
            consumed: false,
        });
        FfiCaskPreview {
            preview_id: id,
            action: action.into(),
            items: records,
            warnings,
            expires_in_seconds: ttl.as_secs(),
            expires_at: unix_seconds(expires_at_wall),
        }
    }

    /// `cask_execute` with injected seams (the test seam).
    pub(crate) fn execute_casks(
        &self,
        preview_id: &str,
        selected_tokens: &[String],
        seams: &Seams<'_>,
        token: &CancellationToken,
        progress: &dyn CaskProgressListener,
    ) -> Result<FfiCaskReport, FfiCaskError> {
        let _gate = self.begin()?;
        token.check()?;
        let path = brew_path(seams)?;
        let (action, items) = self.select_items(preview_id, selected_tokens)?;
        // Refused before the preview is spent; see the module docs.
        if let Some(refusal) = items.iter().find_map(CaskPlanItem::refusal) {
            return Err(failed(refusal));
        }
        let executable = |app: &Path| seams.catalog.executable(app);
        if action == CaskAction::Update {
            check_apps_closed(&items, seams.probe, &executable).map_err(failed)?;
        }
        self.spend_preview(preview_id)?;

        let brew = Brew::new(&path, seams.brew.runner());
        let emit = |step: CaskProgress| progress.on_progress(step.into());
        let results = match action {
            CaskAction::Install => items
                .iter()
                .map(|item| {
                    let outcome = run_install(&brew, item, Some(token.flag()), Some(&emit));
                    (item.token.clone(), outcome)
                })
                .collect(),
            CaskAction::Update => run_updates(
                &brew,
                &items,
                seams.probe,
                &executable,
                Some(token.flag()),
                Some(&emit),
            ),
        };
        Ok(report(action, &items, results))
    }

    /// The selected items of the preview, in its order, once the preview and
    /// the selection check out. Spends nothing.
    fn select_items(
        &self,
        preview_id: &str,
        selected: &[String],
    ) -> Result<(CaskAction, Vec<CaskPlanItem>), FfiCaskError> {
        let invalid = |detail: &str| FfiCaskError::PreviewInvalid {
            detail: detail.into(),
        };
        let state = self.cask_state();
        let preview = state
            .preview
            .as_ref()
            .filter(|preview| preview.id == preview_id)
            .ok_or_else(|| invalid("unknown or replaced preview"))?;
        if preview.consumed {
            return Err(invalid("preview already used"));
        }
        if Instant::now() >= preview.expires_at || SystemTime::now() >= preview.expires_at_wall {
            return Err(invalid("preview expired"));
        }
        if selected.is_empty() {
            return Err(FfiCaskError::InvalidInput {
                detail: "nothing is selected".into(),
            });
        }
        for (index, token) in selected.iter().enumerate() {
            let repeated = selected[..index].contains(token);
            if repeated || !preview.items.iter().any(|item| &item.token == token) {
                return Err(FfiCaskError::InvalidInput {
                    detail: format!("{token} is not in this preview or is selected twice"),
                });
            }
        }
        let items = preview
            .items
            .iter()
            .filter(|item| selected.contains(&item.token))
            .cloned()
            .collect();
        Ok((preview.action, items))
    }

    fn spend_preview(&self, preview_id: &str) -> Result<(), FfiCaskError> {
        let mut state = self.cask_state();
        match state.preview.as_mut().filter(|p| p.id == preview_id) {
            Some(preview) if !preview.consumed => {
                preview.consumed = true;
                Ok(())
            }
            _ => Err(FfiCaskError::PreviewInvalid {
                detail: "preview already used or replaced".into(),
            }),
        }
    }
}

fn report(
    action: CaskAction,
    items: &[CaskPlanItem],
    results: Vec<(String, RunOutcome)>,
) -> FfiCaskReport {
    let count = |keep: fn(&RunOutcome) -> bool| {
        results.iter().filter(|(_, outcome)| keep(outcome)).count() as u64
    };
    FfiCaskReport {
        action: action.into(),
        done_count: count(|o| matches!(o, RunOutcome::Done)),
        failed_count: count(|o| matches!(o, RunOutcome::Failed(_))),
        cancelled: results.iter().any(|(_, outcome)| {
            matches!(outcome, RunOutcome::Cancelled | RunOutcome::NotAttempted)
        }),
        results: results
            .iter()
            .map(|(token, outcome)| {
                let item = items.iter().find(|item| &item.token == token);
                FfiCaskItemResult {
                    token: token.clone(),
                    name: item.map_or_else(|| token.clone(), |item| item.name.clone()),
                    app_path: item.and_then(|item| records::first_app(&item.apps)),
                    outcome: outcome.into(),
                }
            })
            .collect(),
    }
}

fn unix_seconds(time: SystemTime) -> i64 {
    time.duration_since(SystemTime::UNIX_EPOCH)
        .ok()
        .and_then(|since| i64::try_from(since.as_secs()).ok())
        .unwrap_or(i64::MAX)
}
