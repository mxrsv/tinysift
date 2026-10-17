mod checked;
mod ownership;

pub use checked::{
    build_checked_plan, execute_checked_uninstall, CheckedItem, CheckedPlan, ItemResult,
    StopReason, UninstallContext, UninstallRefusal, UninstallReport,
};
pub use ownership::{valid_bundle_id, Consent, LeftoverClass, Ownership};

use crate::engine_error;
use crate::error::{Context, Result};
use crate::options::{SortBy, UninstallOptions as UninstallOpts};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const APPLICATIONS_DIR: &str = "/Applications";
const SYSTEM_BUNDLE_PREFIX: &str = "com.apple.";

/// Apple-managed apps that may appear in /Applications across macOS versions.
/// Used as a fallback guard when CFBundleIdentifier cannot be read (e.g.
/// `defaults` fails inside a sandbox). Match is case-insensitive against
/// the .app file stem.
const SYSTEM_APP_NAMES: &[&str] = &[
    "Safari",
    "Mail",
    "Messages",
    "FaceTime",
    "Calendar",
    "Contacts",
    "Reminders",
    "Notes",
    "Maps",
    "Photos",
    "Music",
    "Podcasts",
    "TV",
    "News",
    "Stocks",
    "Voice Memos",
    "Home",
    "Find My",
    "Books",
    "Calculator",
    "Chess",
    "Dictionary",
    "Image Capture",
    "Photo Booth",
    "Preview",
    "QuickTime Player",
    "Stickies",
    "TextEdit",
    "Time Machine",
    "App Store",
    "System Preferences",
    "System Settings",
    "Siri",
    "Mission Control",
    "Launchpad",
    "Utilities",
    "Automator",
    "Font Book",
    "Migration Assistant",
];

fn is_known_system_app_name(name: &str) -> bool {
    SYSTEM_APP_NAMES
        .iter()
        .any(|sys| sys.eq_ignore_ascii_case(name))
}

// ---------- Data types ----------

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AppEntry {
    pub name: String,
    pub path: PathBuf,
    pub bundle_id: Option<String>,
    pub size: u64,
    pub last_used_days: Option<u64>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Plan {
    pub app: AppEntry,
    pub items: Vec<RemovalItem>,
    pub blocked: Option<String>,
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemovalItem {
    pub path: PathBuf,
    pub size: u64,
    pub kind: ItemKind,
    pub ownership: Ownership,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ItemKind {
    AppBundle,
    Leftover,
}

impl Plan {
    /// Size of everything an explicit selection may remove. Report-only and
    /// uncertain items are listed but never counted.
    pub fn total_size(&self) -> u64 {
        self.items_for(Consent::Explicit).map(|i| i.size).sum()
    }

    /// Items an action taken with `consent` may touch (O2/O5).
    pub fn items_for(&self, consent: Consent) -> impl Iterator<Item = &RemovalItem> {
        self.items
            .iter()
            .filter(move |i| i.ownership.allows(consent))
    }
}

// ---------- App discovery ----------

pub fn list_applications() -> Result<Vec<AppEntry>> {
    Ok(app_paths()?.iter().map(|p| load_app_entry(p)).collect())
}

/// Like `list_applications`, but reads only each app's bundle identifier, for
/// the duplicate-bundle-id check (O5). Size and last use are left empty.
pub fn list_app_identities() -> Result<Vec<AppEntry>> {
    Ok(app_paths()?
        .into_iter()
        .map(|path| AppEntry {
            name: app_name(&path),
            bundle_id: read_bundle_id(&path),
            path,
            size: 0,
            last_used_days: None,
        })
        .collect())
}

fn app_paths() -> Result<Vec<PathBuf>> {
    let entries = fs::read_dir(APPLICATIONS_DIR)
        .with_context(|| format!("failed to read {}", APPLICATIONS_DIR))?;
    Ok(entries
        .flatten()
        .map(|entry| entry.path())
        .filter(|path| path.extension().is_some_and(|e| e == "app"))
        .collect())
}

fn app_name(path: &Path) -> String {
    path.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

fn load_app_entry(path: &Path) -> AppEntry {
    AppEntry {
        name: app_name(path),
        path: path.to_path_buf(),
        bundle_id: read_bundle_id(path),
        size: dir_size(path),
        last_used_days: read_last_used_days(path),
    }
}

pub fn resolve_app_by_name(name: &str) -> Result<AppEntry> {
    let direct = PathBuf::from(APPLICATIONS_DIR).join(format!("{}.app", name));
    if crate::clean::fs_safe::is_dir_safe(&direct) {
        return Ok(load_app_entry(&direct));
    }
    // Case-insensitive fallback
    let entries = fs::read_dir(APPLICATIONS_DIR)
        .with_context(|| format!("failed to read {}", APPLICATIONS_DIR))?;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.extension().map(|e| e == "app").unwrap_or(false) {
            if let Some(stem) = p.file_stem().and_then(|s| s.to_str()) {
                if stem.eq_ignore_ascii_case(name) {
                    return Ok(load_app_entry(&p));
                }
            }
        }
    }
    Err(engine_error!(
        "no app named '{}' found in {}",
        name,
        APPLICATIONS_DIR
    ))
}

fn read_bundle_id(app_path: &Path) -> Option<String> {
    read_info_key(app_path, "CFBundleIdentifier")
}

/// The process name of a running copy of the app (`CFBundleExecutable`), the
/// name `pgrep -x` matches. `None` when it cannot be read.
pub fn read_bundle_executable(app_path: &Path) -> Option<String> {
    read_info_key(app_path, "CFBundleExecutable")
}

fn read_info_key(app_path: &Path, key: &str) -> Option<String> {
    let info = app_path.join("Contents/Info");
    let output = Command::new("defaults")
        .args(["read", info.to_str()?, key])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let s = String::from_utf8(output.stdout).ok()?.trim().to_string();
    if s.is_empty() {
        None
    } else {
        Some(s)
    }
}

fn read_last_used_days(app_path: &Path) -> Option<u64> {
    // mdls -name kMDItemLastUsedDate -raw <path>
    let output = Command::new("mdls")
        .args(["-name", "kMDItemLastUsedDate", "-raw", app_path.to_str()?])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let raw = String::from_utf8(output.stdout).ok()?.trim().to_string();
    parse_mdls_days_ago(&raw)
}

fn parse_mdls_days_ago(raw: &str) -> Option<u64> {
    if raw.is_empty() || raw == "(null)" {
        return None;
    }
    // Format: "2026-04-15 09:12:33 +0000"
    let date_part = raw.split_whitespace().next()?;
    let mut parts = date_part.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    // Days since civil epoch 1970-01-01 using Howard Hinnant's algorithm.
    let last_epoch_day = days_from_civil(year, month, day)?;
    let now_secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .ok()?
        .as_secs() as i64;
    let now_epoch_day = now_secs / 86_400;
    let diff = now_epoch_day - last_epoch_day;
    if diff < 0 {
        Some(0)
    } else {
        Some(diff as u64)
    }
}

fn days_from_civil(y: i64, m: i64, d: i64) -> Option<i64> {
    if !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    Some(era * 146_097 + doe - 719_468)
}

fn dir_size(path: &Path) -> u64 {
    crate::clean::fs_safe::dir_size_safe(path)
}

// ---------- Sorting & picker ----------

pub fn sort_apps(mut apps: Vec<AppEntry>, sort: &SortBy) -> Vec<AppEntry> {
    match sort {
        SortBy::Size => apps.sort_by_key(|a| std::cmp::Reverse(a.size)),
        SortBy::Name => apps.sort_by_key(|a| a.name.to_lowercase()),
        SortBy::LastUsed => apps.sort_by(|a, b| {
            // Larger "days ago" = less recently used = surface first.
            // None (never tracked) ranks last.
            let ka = a.last_used_days.unwrap_or(0);
            let kb = b.last_used_days.unwrap_or(0);
            match (a.last_used_days.is_none(), b.last_used_days.is_none()) {
                (true, false) => std::cmp::Ordering::Greater,
                (false, true) => std::cmp::Ordering::Less,
                _ => kb.cmp(&ka),
            }
        }),
    }
    apps
}

// ---------- Planning ----------

/// `inventory` is the installed-app list used to spot another copy of the same
/// bundle id (O5); `None` means it could not be read.
pub fn build_plan(
    app: &AppEntry,
    opts: &UninstallOpts,
    inventory: Option<&[AppEntry]>,
) -> Result<Plan> {
    let mut items: Vec<RemovalItem> = Vec::new();
    let mut blocked: Option<String> = None;

    match &app.bundle_id {
        Some(bid) if bid.starts_with(SYSTEM_BUNDLE_PREFIX) => {
            blocked = Some(format!("system app ({}) — refuse", bid));
        }
        None if is_known_system_app_name(&app.name) => {
            blocked = Some(format!(
                "looks like a system app ({}) and CFBundleIdentifier could not be read — refuse",
                app.name
            ));
        }
        _ => {}
    }

    if blocked.is_none() && is_brew_cask(&app.name) && !opts.force {
        blocked = Some(format!(
            "looks like a Homebrew cask — use `brew uninstall --cask {}` (or pass --force)",
            app.name.to_lowercase()
        ));
    }

    if !opts.leftovers_only {
        items.push(RemovalItem {
            path: app.path.clone(),
            size: app.size,
            kind: ItemKind::AppBundle,
            ownership: Ownership::AppBundle,
        });
    }

    if !opts.shallow {
        if let Some(home) = crate::clean::trash_plan::account_home() {
            items.extend(ownership::find_leftovers(
                &home,
                app.bundle_id.as_deref(),
                inventory,
                &app.path,
            ));
        }
    }

    Ok(Plan {
        app: app.clone(),
        items,
        blocked,
    })
}

fn is_brew_cask(name: &str) -> bool {
    let lower = name.to_lowercase().replace(' ', "-");
    let path = PathBuf::from("/opt/homebrew/Caskroom").join(&lower);
    path.is_dir()
}

// ---------- Execution ----------

/// Removes the items `consent` allows; report-only and uncertain items are
/// never touched.
pub fn execute_plan(plan: &Plan, hard: bool, consent: Consent) -> Result<()> {
    if let Some(reason) = &plan.blocked {
        return Err(engine_error!("{}", reason));
    }
    for item in plan.items_for(consent) {
        if hard {
            hard_remove(&item.path)?;
        } else {
            move_to_trash(&item.path)?;
        }
    }
    Ok(())
}

fn move_to_trash(path: &Path) -> Result<()> {
    use crate::clean::finder_trash::{FinderTrash, Trash};
    FinderTrash
        .move_to_trash(path)
        .map_err(|e| engine_error!("{}: {e}", path.display()))
}

fn hard_remove(path: &Path) -> Result<()> {
    crate::clean::fs_safe::remove_recursive_safe(path)
        .with_context(|| format!("remove failed: {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_mdls_handles_null_and_empty() {
        assert_eq!(parse_mdls_days_ago(""), None);
        assert_eq!(parse_mdls_days_ago("(null)"), None);
    }

    #[test]
    fn parse_mdls_extracts_days() {
        // 1970-01-01 → epoch day 0; today is many days later, must be >= 20000.
        let days = parse_mdls_days_ago("1970-01-01 00:00:00 +0000").unwrap();
        assert!(days >= 20_000);
    }

    #[test]
    fn sort_size_desc() {
        let apps = vec![mk("A", 100), mk("B", 300), mk("C", 200)];
        let s = sort_apps(apps, &SortBy::Size);
        assert_eq!(s[0].name, "B");
        assert_eq!(s[2].name, "A");
    }

    fn item(name: &str, ownership: Ownership) -> RemovalItem {
        RemovalItem {
            path: PathBuf::from(name),
            size: 10,
            kind: ItemKind::Leftover,
            ownership,
        }
    }

    #[test]
    fn items_for_filters_by_consent_and_total_excludes_unremovable() {
        let plan = Plan {
            app: mk("A", 0),
            blocked: None,
            items: vec![
                item(
                    "cache",
                    Ownership::Owned {
                        class: LeftoverClass::Regenerable,
                    },
                ),
                item(
                    "data",
                    Ownership::Owned {
                        class: LeftoverClass::DataRisk,
                    },
                ),
                item("shared", Ownership::ReportOnly { reason: "r".into() }),
                item("odd", Ownership::Uncertain { reason: "r".into() }),
            ],
        };
        let paths = |c| {
            plan.items_for(c)
                .map(|i| i.path.to_str().unwrap().to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(paths(Consent::Unattended), ["cache"]);
        assert_eq!(paths(Consent::Explicit), ["cache", "data"]);
        assert_eq!(plan.total_size(), 20);
    }

    fn mk(name: &str, size: u64) -> AppEntry {
        AppEntry {
            name: name.to_string(),
            path: PathBuf::from(format!("/Applications/{}.app", name)),
            bundle_id: None,
            size,
            last_used_days: None,
        }
    }
}
