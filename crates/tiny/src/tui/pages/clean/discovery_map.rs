//! How one CLI discovery becomes the categories the page lists: which route
//! each takes, what was found, and what turns cleaning off. Pure, so every
//! route and precedence is tested without reading the machine (C7).

use tiny_core::clean::discover::CliDiscovery;
use tiny_core::clean::providers::{comes_back, desktop_report_only, CleanProvider, ReportOnly};

use super::engine::{Category, Finding, Route};

/// Said when the engine refused cleanup without naming a reason.
pub const UNNAMED_FAILURE: &str = "a category could not be read";

/// A category's route from the engine's report-only answer; `None` for a
/// Destructive category, which the TUI never lists (TU10).
pub fn route(report_only: Option<ReportOnly>) -> Option<Route> {
    match report_only {
        None => Some(Route::Trash),
        Some(ReportOnly::NotPerPathTrash) => Some(Route::ForGood),
        Some(ReportOnly::Destructive) => None,
        Some(
            ReportOnly::UnreliableMatch
            | ReportOnly::NeedsAdministrator
            | ReportOnly::XcodeNotInstalled,
        ) => Some(Route::ReportOnly),
    }
}

/// What the read said of category `id`: a failure first (it turns cleaning
/// off even when items were found), then what was found, then the app that
/// kept it from being read. `None` when it found nothing and is not listed.
pub fn finding(id: &str, found: &CliDiscovery) -> Option<Finding> {
    if let Some((_, reason)) = found.failed_categories.iter().find(|(c, _)| c == id) {
        return Some(Finding::Failed {
            reason: reason.clone(),
        });
    }
    if let Some(group) = found.report.groups.iter().find(|g| g.id == id) {
        return Some(Finding::Found(group.clone()));
    }
    found
        .report
        .skipped_running
        .iter()
        .find(|(c, _)| c == id)
        .map(|(_, app)| Finding::AppRunning { app: app.clone() })
}

/// Why the engine refuses cleanup after this read, if it does: its first
/// diagnostic, as `sift clean` prints it.
pub fn failure(found: &CliDiscovery) -> Option<String> {
    found.failed.then(|| {
        found
            .diagnostics
            .first()
            .cloned()
            .unwrap_or_else(|| UNNAMED_FAILURE.into())
    })
}

/// The listed categories, in the providers' order.
pub fn categories(providers: &[Box<dyn CleanProvider>], found: &CliDiscovery) -> Vec<Category> {
    providers
        .iter()
        .filter_map(|provider| {
            let id = provider.id();
            let route = route(desktop_report_only(provider.as_ref()))?;
            let finding = finding(id, found)?;
            Some(Category {
                id: id.to_string(),
                label: provider.label().to_string(),
                risk: provider.risk(),
                reason: provider.inclusion_reason(),
                comes_back: comes_back(id),
                route,
                finding,
            })
        })
        .collect()
}

#[cfg(test)]
#[path = "discovery_map_tests.rs"]
mod tests;
