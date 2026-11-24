//! The page's one way into the engine. [`CoreEngine`] calls `tiny-core` in
//! the order `sift uninstall` does (`render/uninstall.rs`): list the apps,
//! build each plan against that list, freeze it with `prepare_plan` before the
//! action menu opens, and remove through `execute_prepared_plan`, the checked
//! executor. The page holds no rule of its own about what may be removed.

use std::path::PathBuf;

use tiny_core::options::UninstallOptions;
use tiny_core::uninstall::{self, AppEntry, CheckedPlan, Consent, Plan};

/// What the page asks of the engine. Every call may be slow; the page makes
/// them on worker threads only.
pub trait UninstallEngine: Send + Sync + 'static {
    /// A plan frozen for execution: identity and paths taken before consent.
    type Frozen: Send + 'static;

    /// Every app in `/Applications`, with size, last use and bundle identifier.
    fn list_apps(&self) -> Result<Vec<AppEntry>, String>;

    /// The engine's reason for refusing to remove `app` at all, if it does.
    fn refusal(&self, app: &AppEntry) -> Option<String>;

    /// What removing `app` lists. `installed` is the app list, which the
    /// engine uses to spot a second copy of the same bundle identifier.
    fn plan(&self, app: &AppEntry, installed: &[AppEntry]) -> Result<Plan, String>;

    /// Freezes `plan` for the checked executor.
    fn freeze(&self, plan: &Plan) -> Result<Self::Frozen, String>;

    /// Removes what `consent` allows of a frozen plan, to the Trash or for good.
    fn remove(&self, frozen: &Self::Frozen, hard: bool, consent: Consent) -> Result<(), String>;

    /// The account's `~/Library`, so leftover paths can be shown without it.
    fn library(&self) -> Option<PathBuf>;
}

/// The options `sift uninstall` runs with when no flag is given: the app
/// and its leftovers, Homebrew casks refused.
const OPTIONS: UninstallOptions = UninstallOptions {
    shallow: false,
    leftovers_only: false,
    force: false,
};

/// The real engine.
pub struct CoreEngine;

impl UninstallEngine for CoreEngine {
    type Frozen = CheckedPlan;

    fn list_apps(&self) -> Result<Vec<AppEntry>, String> {
        uninstall::list_applications().map_err(|error| error.to_string())
    }

    fn refusal(&self, app: &AppEntry) -> Option<String> {
        uninstall::blocked_reason(app, OPTIONS.force)
    }

    fn plan(&self, app: &AppEntry, installed: &[AppEntry]) -> Result<Plan, String> {
        uninstall::build_plan(app, &OPTIONS, Some(installed)).map_err(|error| error.to_string())
    }

    fn freeze(&self, plan: &Plan) -> Result<CheckedPlan, String> {
        uninstall::prepare_plan(plan).map_err(|error| error.to_string())
    }

    fn remove(&self, frozen: &CheckedPlan, hard: bool, consent: Consent) -> Result<(), String> {
        uninstall::execute_prepared_plan(frozen, hard, consent).map_err(|error| error.to_string())
    }

    fn library(&self) -> Option<PathBuf> {
        tiny_core::clean::trash_plan::account_home().map(|home| home.join("Library"))
    }
}
