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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ItemKind {
    AppBundle,
    Leftover,
}

impl Plan {
    pub fn total_size(&self) -> u64 {
        self.items.iter().map(|i| i.size).sum()
    }
}

// ---------- App discovery ----------

pub fn list_applications() -> Result<Vec<AppEntry>> {
    let mut apps = Vec::new();
    let entries = fs::read_dir(APPLICATIONS_DIR)
        .with_context(|| format!("failed to read {}", APPLICATIONS_DIR))?;

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().map(|e| e == "app").unwrap_or(false) {
            apps.push(load_app_entry(&path));
        }
    }
    Ok(apps)
}

fn load_app_entry(path: &Path) -> AppEntry {
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string());
    AppEntry {
        name,
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
    let info = app_path.join("Contents/Info");
    let output = Command::new("defaults")
        .args(["read", info.to_str()?, "CFBundleIdentifier"])
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

pub fn build_plan(app: &AppEntry, opts: &UninstallOpts) -> Result<Plan> {
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
        });
    }

    if !opts.shallow {
        let leftovers = find_leftovers(&app.name, app.bundle_id.as_deref());
        items.extend(leftovers);
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

fn find_leftovers(_app_name: &str, bundle_id: Option<&str>) -> Vec<RemovalItem> {
    let mut items = Vec::new();
    let home = match std::env::var_os("HOME").map(PathBuf::from) {
        Some(h) => h,
        None => return items,
    };

    // Only scan when we have a bundle ID. Name-keyed fallback was removed
    // because it can collide with system folders for apps with generic names
    // (e.g. "Mail", "Notes" → /Library/Application Support/<system-folder>).
    let bid = match bundle_id {
        Some(b) => b,
        None => return items,
    };

    let mut candidates: Vec<PathBuf> = Vec::new();
    candidates.push(home.join("Library/Application Support").join(bid));
    candidates.push(home.join("Library/Caches").join(bid));
    candidates.push(
        home.join("Library/Preferences")
            .join(format!("{}.plist", bid)),
    );
    candidates.push(home.join("Library/Containers").join(bid));
    candidates.push(
        home.join("Library/Saved Application State")
            .join(format!("{}.savedState", bid)),
    );
    candidates.push(home.join("Library/HTTPStorages").join(bid));
    candidates.push(
        home.join("Library/HTTPStorages")
            .join(format!("{}.binarycookies", bid)),
    );
    candidates.push(home.join("Library/WebKit").join(bid));

    // LaunchAgents pattern <bundle_id>*.plist
    if let Ok(entries) = fs::read_dir(home.join("Library/LaunchAgents")) {
        for e in entries.flatten() {
            let p = e.path();
            if let Some(stem) = p.file_name().and_then(|s| s.to_str()) {
                if stem.starts_with(bid) {
                    candidates.push(p);
                }
            }
        }
    }
    // Group Containers — substring match on bundle id
    if let Ok(entries) = fs::read_dir(home.join("Library/Group Containers")) {
        for e in entries.flatten() {
            let p = e.path();
            if let Some(stem) = p.file_name().and_then(|s| s.to_str()) {
                if stem.contains(bid) {
                    candidates.push(p);
                }
            }
        }
    }

    for path in candidates {
        if !path.exists() {
            continue;
        }
        let size = if crate::clean::fs_safe::is_dir_safe(&path) {
            dir_size(&path)
        } else {
            fs::symlink_metadata(&path).map(|m| m.len()).unwrap_or(0)
        };
        items.push(RemovalItem {
            path,
            size,
            kind: ItemKind::Leftover,
        });
    }
    items
}

// ---------- Execution ----------

pub fn execute_plan(plan: &Plan, hard: bool) -> Result<()> {
    if let Some(reason) = &plan.blocked {
        return Err(engine_error!("{}", reason));
    }
    for item in &plan.items {
        if hard {
            hard_remove(&item.path)?;
        } else {
            move_to_trash(&item.path)?;
        }
    }
    Ok(())
}

fn move_to_trash(path: &Path) -> Result<()> {
    let posix = path
        .to_str()
        .ok_or_else(|| engine_error!("non-utf8 path: {}", path.display()))?;
    // Pass the path as `argv` (item 1 of argv) instead of interpolating it
    // into the script. Avoids any AppleScript escaping concerns even if the
    // path contains backslashes, quotes, or newlines.
    let script = "on run argv\n\
                  tell application \"Finder\" to delete (POSIX file (item 1 of argv) as alias)\n\
                  end run";
    let output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .arg(posix)
        .output()
        .with_context(|| format!("failed to spawn osascript for {}", path.display()))?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(engine_error!(
            "osascript failed for {}: {}",
            path.display(),
            err
        ));
    }
    Ok(())
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
