//! The words and figures the Clean page prints, from engine values.

use std::path::{Path, PathBuf};
use std::time::Duration;

use tiny_core::clean::types::{ComesBack, Evidence};

use crate::tui::widgets::size_format::format_count;
use crate::tui::widgets::text_fit::cut;

const DAY: i64 = 86_400;

/// `1 category`, `5 categories`, `1,284 paths`.
pub fn plural(count: usize, one: &str, many: &str) -> String {
    let word = if count == 1 { one } else { many };
    format!("{} {word}", format_count(count as u64))
}

/// When a category's items come back after a clean.
pub fn comes_back(back: Option<ComesBack>) -> &'static str {
    match back {
        Some(ComesBack::Rebuild { .. }) => "when you build again",
        Some(ComesBack::Redownload) => "downloaded when needed",
        Some(ComesBack::AppRecreates) => "on its own",
        Some(ComesBack::TrashOnly | ComesBack::NotRecoverable) | None => "never",
    }
}

/// `today`, `1 day ago`, `214 days ago`, counted from the read.
pub fn age(read_at: i64, at: i64) -> String {
    match read_at.saturating_sub(at) / DAY {
        days if days <= 0 => "today".into(),
        1 => "1 day ago".into(),
        days => format!("{} days ago", format_count(days.unsigned_abs())),
    }
}

/// A path with the home folder written `~`.
pub fn shown_path(path: &Path, home: Option<&PathBuf>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// `4.1s` under a minute, then `2m 05s`.
pub fn seconds(elapsed: Duration) -> String {
    let tenths = elapsed.as_millis() / 100;
    if tenths < 600 {
        format!("{}.{}s", tenths / 10, tenths % 10)
    } else {
        let whole = elapsed.as_secs();
        format!("{}m {:02}s", whole / 60, whole % 60)
    }
}

/// Up to `shown` names, each cut to `width`, then `and N more`.
pub fn some_names(names: &[&str], shown: usize, width: usize) -> String {
    let total = names.len();
    let listed = names
        .iter()
        .take(shown)
        .map(|name| cut(name, width))
        .collect::<Vec<_>>()
        .join(", ");
    if total > shown {
        format!("{listed} and {} more", total - shown)
    } else {
        listed
    }
}

/// The engine's reason without the `kept: ` it starts with.
pub fn kept_reason(raw: &str) -> &str {
    raw.strip_prefix("kept: ").unwrap_or(raw)
}

/// The facts the engine gave for a path, as `(label, value)`.
pub fn facts(evidence: &[Evidence], read_at: i64) -> Vec<(String, String)> {
    evidence
        .iter()
        .filter_map(|fact| {
            let (label, value) = match fact {
                Evidence::Modified { at } => ("Modified".into(), age(read_at, *at)),
                Evidence::ManifestModified { file, at } => {
                    (file.clone(), format!("edited {}", age(read_at, *at)))
                }
                Evidence::LastCommit { at } => ("Last commit".into(), age(read_at, *at)),
                Evidence::WorkTreeClean => ("Work tree".into(), "clean".into()),
                Evidence::NotGitRepo => ("Git".into(), "not a repository".into()),
                Evidence::VenvMarker => ("Virtualenv".into(), "pyvenv.cfg inside".into()),
                Evidence::LastOpened { at } => ("Last opened".into(), age(read_at, *at)),
                Evidence::OwningApp { name } => ("App".into(), name.clone()),
                Evidence::OwningAppUnknown => ("App".into(), "unknown".into()),
                Evidence::Sensitive { reason } => ("Looks private".into(), reason.clone()),
                Evidence::SimulatorUnavailable { name, .. } => ("Simulator".into(), name.clone()),
                Evidence::SimulatorRuntime { version, .. } => ("Runtime".into(), version.clone()),
                Evidence::BrowserProfile {
                    browser,
                    profile_name,
                    ..
                } => ("Profile".into(), format!("{browser} · {profile_name}")),
                Evidence::TrashEntry { .. } | Evidence::TrashRest { .. } => return None,
            };
            Some((label, value))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn words_match_the_mock() {
        assert_eq!(plural(1, "category", "categories"), "1 category");
        assert_eq!(plural(1284, "path", "paths"), "1,284 paths");
        assert_eq!(age(300 * DAY, 86 * DAY), "214 days ago");
        assert_eq!(age(10, 5), "today");
        assert_eq!(age(DAY, 0), "1 day ago");
        assert_eq!(seconds(Duration::from_millis(9_400)), "9.4s");
        assert_eq!(seconds(Duration::from_secs(125)), "2m 05s");
        let names = [
            "Xcode DerivedData",
            "node_modules (idle)",
            "pnpm store",
            "a",
            "b",
        ];
        assert_eq!(
            some_names(&names, 3, 28),
            "Xcode DerivedData, node_modules (idle), pnpm store and 2 more"
        );
        assert_eq!(kept_reason("kept: Xcode is running"), "Xcode is running");
    }

    #[test]
    fn the_home_folder_is_a_tilde() {
        let home = PathBuf::from("/Users/you");
        assert_eq!(
            shown_path(Path::new("/Users/you/.npm/_cacache"), Some(&home)),
            "~/.npm/_cacache"
        );
        assert_eq!(
            shown_path(Path::new("/Library/Logs"), Some(&home)),
            "/Library/Logs"
        );
        assert_eq!(
            shown_path(Path::new("/Users/youth"), Some(&home)),
            "/Users/youth"
        );
    }
}
