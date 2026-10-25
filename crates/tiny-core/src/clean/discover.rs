//! Build the active provider list from CLI options and run discovery.
//!
//! Risk gating:
//! - `--category` overrides risk filtering. Naming a category counts as
//!   explicit consent; `--include-*` flags are ignored when `--category` is
//!   set.
//! - Otherwise: Safe by default; `--include-review` adds Review;
//!   `--include-destructive` adds Destructive.

use crate::error::Result;
use std::path::PathBuf;
use std::sync::Arc;

use super::freed_size::SharedBlocks;
use super::process::{AppProbe, PgrepChecker, ProcessChecker};
use super::providers::{all_providers, all_providers_with, CleanProvider};
use super::scan_context::{ScanContext, Unreadable};
use super::trash_plan::protected_reason;
use super::types::{CleanItem, RiskLevel};
use crate::options::CleanOptions as CleanOpts;
use crate::runner::CommandRunner;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CategoryGroup {
    pub id: String,
    pub label: String,
    pub risk: RiskLevel,
    pub items: Vec<CleanItem>,
    pub total_size: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveryReport {
    pub groups: Vec<CategoryGroup>,
    pub skipped_running: Vec<(String, String)>, // (category_id, app)
}

/// A discovered item plus what its size walk could not read; with
/// `unreadable` set, `item.size` is a lower bound.
#[derive(Debug, Clone)]
pub struct CheckedItem {
    pub item: CleanItem,
    pub unreadable: Option<Unreadable>,
    /// The volume could not tell which blocks other files share, so
    /// `item.size` counts them all: deleting frees at most that.
    pub size_is_upper_bound: bool,
    /// Blocks the item shares with files outside it, which `item.size`
    /// leaves out; several items removed together may free them.
    pub shared: SharedBlocks,
}

#[derive(Debug, Clone)]
pub enum CategoryOutcome {
    Found {
        items: Vec<CheckedItem>,
        /// Listing/search paths with unreadable entries, outside any item.
        unreadable: Vec<(PathBuf, Unreadable)>,
        /// Directories discovery listed or walked for this category.
        roots: Vec<PathBuf>,
        /// Paths left out, with why: they must never be cleaned (desktop
        /// discovery only, `trash_plan::protected_reason`), or a safety
        /// check could not answer (`ScanContext::note_refused`).
        refused: Vec<(PathBuf, String)>,
    },
    /// The owning app is running, so the category was not scanned.
    AppRunning { app: String },
    /// A required tool or safety probe is unavailable.
    Unavailable { reason: String },
    /// The provider failed; other categories are unaffected.
    Failed { error: String },
}

#[derive(Debug, Clone)]
pub struct CheckedCategory {
    pub id: String,
    pub label: String,
    /// Why the category's items are candidates (`CleanProvider::inclusion_reason`).
    pub inclusion_reason: String,
    pub risk: RiskLevel,
    pub outcome: CategoryOutcome,
}

#[derive(Debug, Clone)]
pub struct CheckedDiscovery {
    pub categories: Vec<CheckedCategory>,
    /// Discovery stopped early; `categories` is incomplete.
    pub cancelled: bool,
}

/// Returns the providers selected by the CLI options. Filtering rules from
/// the plan flag matrix. Categories that need administrator rights come
/// only when named: the CLI never runs `sudo` and refuses their items.
pub fn select_providers(opts: &CleanOpts) -> Vec<Box<dyn CleanProvider>> {
    let mut providers = filter_providers(opts, all_providers(opts));
    if opts.category.is_empty() {
        providers.retain(|p| !p.needs_admin());
    }
    providers
}

/// `select_providers` with an explicit runner for tool-backed providers.
pub fn select_providers_with(
    opts: &CleanOpts,
    runner: Arc<dyn CommandRunner>,
) -> Vec<Box<dyn CleanProvider>> {
    filter_providers(opts, all_providers_with(opts, runner))
}

fn filter_providers(
    opts: &CleanOpts,
    all: Vec<Box<dyn CleanProvider>>,
) -> Vec<Box<dyn CleanProvider>> {
    if !opts.category.is_empty() {
        return all
            .into_iter()
            .filter(|p| opts.category.iter().any(|c| c == p.id()))
            .collect();
    }
    all.into_iter()
        .filter(|p| match p.risk() {
            RiskLevel::Safe => true,
            RiskLevel::Review => opts.include_review,
            RiskLevel::Destructive => {
                opts.include_destructive || (opts.include_trash && p.desktop_empties_trash())
            }
        })
        .collect()
}

pub fn validate_options(opts: &CleanOpts) -> Result<()> {
    if opts.idle_days == 0 {
        return Err(crate::error::Error::InvalidInput(
            "idleDays must be greater than zero".into(),
        ));
    }
    Ok(())
}

/// CLI diagnostics kept outside the legacy serialized report shape.
pub struct CliDiscovery {
    pub report: DiscoveryReport,
    pub diagnostics: Vec<String>,
    pub failed: bool,
}

pub fn discover_for_cli(opts: &CleanOpts) -> Result<CliDiscovery> {
    validate_options(opts)?;
    let providers = select_providers(opts);
    let probe = super::process::RunnerProbe(&crate::runner::RealRunner);
    let ctx = ScanContext::new(None, &probe);
    let checked = discover_checked(&providers, &ctx, None);
    Ok(cli_discovery(checked, opts, &providers))
}

fn cli_discovery(
    checked: CheckedDiscovery,
    opts: &CleanOpts,
    providers: &[Box<dyn CleanProvider>],
) -> CliDiscovery {
    let mut diagnostics = Vec::new();
    let mut failed = false;
    for category in &checked.categories {
        match &category.outcome {
            CategoryOutcome::Failed { error } => {
                failed = true;
                diagnostics.push(format!("{}: scan failed: {error}", category.id));
            }
            CategoryOutcome::Unavailable { reason } => {
                let optional_tool_missing = providers
                    .iter()
                    .find(|provider| provider.id() == category.id)
                    .and_then(|provider| provider.required_tool())
                    .is_some_and(|tool| reason == &format!("{tool} was not found"));
                failed |= !opts.category.is_empty() || !optional_tool_missing;
                diagnostics.push(format!("{}: unavailable: {reason}", category.id));
            }
            CategoryOutcome::AppRunning { app } if !opts.category.is_empty() => {
                failed = true;
                diagnostics.push(format!("{}: not scanned: {app} is running", category.id));
            }
            CategoryOutcome::Found {
                unreadable,
                refused,
                items,
                ..
            } => {
                let partial = unreadable.len()
                    + items
                        .iter()
                        .filter(|item| item.unreadable.is_some())
                        .count();
                if partial > 0 {
                    failed = true;
                    diagnostics.push(format!(
                        "{}: {partial} paths could not be fully read",
                        category.id
                    ));
                }
                if !refused.is_empty() {
                    diagnostics.push(format!(
                        "{}: {} paths refused by safety checks",
                        category.id,
                        refused.len()
                    ));
                }
            }
            _ => {}
        }
    }
    CliDiscovery {
        report: legacy_report(checked),
        diagnostics,
        failed,
    }
}

pub fn discover(opts: &CleanOpts) -> Result<DiscoveryReport> {
    discover_with_progress(opts, None)
}

pub fn discover_with_checker(
    opts: &CleanOpts,
    checker: &dyn ProcessChecker,
) -> Result<DiscoveryReport> {
    discover_inner(opts, checker, None)
}

pub fn discover_with_progress(
    opts: &CleanOpts,
    progress: crate::progress::ProgressCallback<'_>,
) -> Result<DiscoveryReport> {
    validate_options(opts)?;
    discover_inner(opts, &PgrepChecker, progress)
}

/// Lenient probe for the compatibility path: a checker that cannot fail.
struct Lenient<'a>(&'a dyn ProcessChecker);

impl AppProbe for Lenient<'_> {
    fn probe(&self, name: &str) -> std::result::Result<bool, String> {
        Ok(self.0.is_running(name))
    }
}

/// The CLI's discovery. Unavailable and failed categories are dropped,
/// which is what the CLI showed before failures were reported.
fn discover_inner(
    opts: &CleanOpts,
    checker: &dyn ProcessChecker,
    progress: crate::progress::ProgressCallback<'_>,
) -> Result<DiscoveryReport> {
    let providers = select_providers(opts);
    let probe = Lenient(checker);
    let ctx = ScanContext::new(None, &probe);
    Ok(legacy_report(discover_checked(&providers, &ctx, progress)))
}

/// Maps checked discovery to the CLI's report: unavailable, failed and
/// empty categories are dropped, as the CLI showed nothing for them before.
fn legacy_report(checked: CheckedDiscovery) -> DiscoveryReport {
    let mut report = DiscoveryReport {
        groups: Vec::new(),
        skipped_running: Vec::new(),
    };
    for category in checked.categories {
        match category.outcome {
            CategoryOutcome::Found { items, .. } if !items.is_empty() => {
                let items: Vec<CleanItem> = items.into_iter().map(|c| c.item).collect();
                let total_size = items
                    .iter()
                    .fold(0u64, |sum, item| sum.saturating_add(item.size));
                report.groups.push(CategoryGroup {
                    id: category.id,
                    label: category.label,
                    risk: category.risk,
                    items,
                    total_size,
                });
            }
            CategoryOutcome::AppRunning { app } => {
                report.skipped_running.push((category.id, app));
            }
            _ => {}
        }
    }
    report
}

/// Runs every provider, isolating failures: a provider that errors, lacks
/// its tool or cannot check its app becomes a typed entry and the rest
/// still run. Stops between and inside providers once `ctx` is cancelled.
pub fn discover_checked(
    providers: &[Box<dyn CleanProvider>],
    ctx: &ScanContext<'_>,
    progress: crate::progress::ProgressCallback<'_>,
) -> CheckedDiscovery {
    let total = providers.len() as u64;
    let mut categories = Vec::new();
    for (index, provider) in providers.iter().enumerate() {
        if ctx.is_cancelled() {
            return CheckedDiscovery {
                categories,
                cancelled: true,
            };
        }
        crate::progress::report(
            progress,
            "scan",
            index as u64,
            Some(total),
            provider.label(),
        );
        let outcome = discover_one(provider.as_ref(), ctx);
        let findings = ctx.take_findings();
        if ctx.is_cancelled() {
            // The provider's walk stopped early; its result is partial.
            return CheckedDiscovery {
                categories,
                cancelled: true,
            };
        }
        let outcome = match outcome {
            Some(Ok(items)) => found(items, findings, |path| {
                let home = ctx.protected_home()?;
                protected_reason(path, Some(home), provider.roots_from_tool_output())
            }),
            Some(Err(outcome)) => outcome,
            None => continue,
        };
        // Empty categories stay: the desktop lists what was checked and found nothing.
        categories.push(CheckedCategory {
            id: provider.id().to_string(),
            label: provider.label().to_string(),
            inclusion_reason: provider.inclusion_reason(),
            risk: provider.risk(),
            outcome,
        });
    }
    crate::progress::report(progress, "scan", total, Some(total), "Discovery complete");
    CheckedDiscovery {
        categories,
        cancelled: false,
    }
}

/// `None`: the provider does not apply here (nothing to report).
fn discover_one(
    provider: &dyn CleanProvider,
    ctx: &ScanContext<'_>,
) -> Option<std::result::Result<Vec<CleanItem>, CategoryOutcome>> {
    if !provider.available() {
        let tool = provider.required_tool()?;
        return Some(Err(CategoryOutcome::Unavailable {
            reason: format!("{tool} was not found"),
        }));
    }
    for app in provider.quit_apps() {
        match ctx.app_running(app) {
            Ok(false) => {}
            Ok(true) => {
                return Some(Err(CategoryOutcome::AppRunning {
                    app: app.to_string(),
                }))
            }
            Err(e) => {
                return Some(Err(CategoryOutcome::Unavailable {
                    reason: e.to_string(),
                }))
            }
        }
    }
    Some(provider.discover(ctx).map_err(|e| CategoryOutcome::Failed {
        error: e.to_string(),
    }))
}

fn found(
    items: Vec<CleanItem>,
    findings: super::scan_context::ScanFindings,
    protected: impl Fn(&std::path::Path) -> Option<String>,
) -> CategoryOutcome {
    let mut unreadable = findings.unreadable;
    let mut shared = findings.shared;
    let mut refused: Vec<(PathBuf, String)> = findings.refused.into_iter().collect();
    let mut kept = Vec::new();
    for item in items {
        match protected(&item.path) {
            Some(reason) => refused.push((item.path, reason)),
            None => kept.push(CheckedItem {
                unreadable: unreadable.remove(&item.path),
                size_is_upper_bound: findings.size_upper_bound.contains(&item.path),
                shared: shared.remove(&item.path).unwrap_or_default(),
                item,
            }),
        }
    }
    CategoryOutcome::Found {
        items: kept,
        unreadable: unreadable.into_iter().collect(),
        roots: findings.roots.into_iter().collect(),
        refused,
    }
}

#[cfg(test)]
mod tests;
