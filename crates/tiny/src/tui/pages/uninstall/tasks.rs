//! The page's work off the UI thread: read the list, read one plan, freeze
//! the picked plans, run the removal app by app. Each job answers with one
//! [`Msg`]; an engine panic comes back as that job's failure, so a run never
//! loses track of the app it was on.

use std::any::Any;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::PathBuf;
use std::sync::Arc;

use tiny_core::uninstall::{AppEntry, Plan};

use super::engine::UninstallEngine;
use super::model::{AppRow, MENU_CONSENT};
use crate::tui::work::Worker;

/// A picked app whose plan is frozen for the checked executor.
pub struct FrozenApp<F> {
    pub plan: Plan,
    pub frozen: F,
}

/// One picked app after freezing: its frozen plan, or its name and the
/// engine's refusal.
pub type FreezeOutcome<F> = Result<FrozenApp<F>, (String, String)>;

/// What the page's tasks send back. `ticket` ties an answer to the read that
/// asked for it, so a late answer to an older read is dropped.
pub enum Msg<F> {
    Listed {
        ticket: u64,
        result: Result<Vec<AppRow>, String>,
    },
    Planned {
        ticket: u64,
        path: PathBuf,
        result: Result<Plan, String>,
    },
    /// One entry per picked app, in list order: frozen, or the app's name and
    /// the engine's refusal.
    Frozen {
        ticket: u64,
        apps: Vec<FreezeOutcome<F>>,
    },
    /// The run began removing the app at this index.
    Started(usize),
    Finished(usize, Result<(), String>),
    /// The run is over: every app ran, or it stopped between two apps.
    RunEnded,
}

/// Reads `/Applications` and the engine's refusal of each app.
pub fn list<E: UninstallEngine>(
    engine: Arc<E>,
    ticket: u64,
) -> impl FnOnce(Worker<Msg<E::Frozen>>) {
    move |worker| {
        let result = guarded(|| {
            Ok(engine
                .list_apps()?
                .into_iter()
                .map(|app| AppRow {
                    refusal: engine.refusal(&app),
                    app,
                })
                .collect())
        });
        worker.send(Msg::Listed { ticket, result });
    }
}

/// Builds the plan of one app, read-only.
pub fn plan<E: UninstallEngine>(
    engine: Arc<E>,
    ticket: u64,
    app: AppEntry,
    installed: Vec<AppEntry>,
) -> impl FnOnce(Worker<Msg<E::Frozen>>) {
    move |worker| {
        let result = guarded(|| engine.plan(&app, &installed));
        worker.send(Msg::Planned {
            ticket,
            path: app.path,
            result,
        });
    }
}

/// Builds each picked app's plan again and freezes it, as the command does
/// right before its menu. Nothing is removed here.
pub fn freeze<E: UninstallEngine>(
    engine: Arc<E>,
    ticket: u64,
    picked: Vec<AppEntry>,
    installed: Vec<AppEntry>,
) -> impl FnOnce(Worker<Msg<E::Frozen>>) {
    move |worker| {
        let apps = picked
            .into_iter()
            .map(|app| {
                guarded(|| {
                    let plan = engine.plan(&app, &installed)?;
                    let frozen = engine.freeze(&plan)?;
                    Ok(FrozenApp { plan, frozen })
                })
                .map_err(|reason| (app.name.clone(), reason))
            })
            .collect();
        worker.send(Msg::Frozen { ticket, apps });
    }
}

/// Removes the frozen apps one by one with the menu's consent. A stop asked
/// for while an app is removed takes effect before the next one.
pub fn run<E: UninstallEngine>(
    engine: Arc<E>,
    batch: Vec<FrozenApp<E::Frozen>>,
    hard: bool,
) -> impl FnOnce(Worker<Msg<E::Frozen>>) {
    move |worker| {
        for (index, app) in batch.iter().enumerate() {
            if worker.is_cancelled() || !worker.send(Msg::Started(index)) {
                break;
            }
            let result = guarded(|| engine.remove(&app.frozen, hard, MENU_CONSENT));
            if !worker.send(Msg::Finished(index, result)) {
                break;
            }
        }
        worker.send(Msg::RunEnded);
    }
}

/// `job`, with a panic turned into its failure.
fn guarded<T>(job: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    catch_unwind(AssertUnwindSafe(job)).unwrap_or_else(|payload| Err(panic_text(payload.as_ref())))
}

fn panic_text(payload: &(dyn Any + Send)) -> String {
    let text = payload
        .downcast_ref::<&str>()
        .map(|text| text.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned());
    format!(
        "stopped unexpectedly: {}",
        text.as_deref().unwrap_or("no reason given")
    )
}
