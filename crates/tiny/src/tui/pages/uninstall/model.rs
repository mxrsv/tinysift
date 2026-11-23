//! What the page shows of the engine's answers: rows, entry labels, short
//! paths and the figures of a plan. Labels follow `sift uninstall`'s report;
//! what may be removed is always the engine's `Ownership::allows`.

use std::path::{Component, Path};

use tiny_core::uninstall::{AppEntry, Consent, LeftoverClass, Ownership, Plan, RemovalItem};

use crate::tui::widgets::size_format::format_count;

/// The consent the command's interactive menu gives Move to Trash and Hard
/// delete (`render/uninstall.rs`, `Consent::Explicit` when `-y` is not given):
/// the app, `cache` and `data` entries. Figures shown and removals run use it.
pub const MENU_CONSENT: Consent = Consent::Explicit;

/// One app of the list with the engine's refusal, read once with the list.
#[derive(Clone, Debug)]
pub struct AppRow {
    pub app: AppEntry,
    pub refusal: Option<String>,
}

impl AppRow {
    pub fn refused(&self) -> bool {
        self.refusal.is_some()
    }
}

/// The few words at the right of a refused row; the detail shows the engine's own.
pub fn short_refusal(reason: &str) -> &str {
    if reason.contains("system app") {
        "Apple app"
    } else if reason.contains("Homebrew cask") {
        "brew cask"
    } else {
        reason
    }
}

/// `today`, `298d ago`, `2,911d ago`, or `never` when macOS has no date.
pub fn last_used(days: Option<u64>) -> String {
    match days {
        Some(0) => "today".into(),
        Some(days) => format!("{}d ago", format_count(days)),
        None => "never".into(),
    }
}

/// `1 app`, `2 apps`, `1 entry`, `3 entries`.
pub fn plural(count: usize, one: &str, many: &str) -> String {
    let word = if count == 1 { one } else { many };
    format!("{} {word}", format_count(count as u64))
}

/// The command's label for an entry (`app`, `cache`, `data`); entries the
/// engine only reports are `kept`.
pub fn label(item: &RemovalItem) -> &'static str {
    match &item.ownership {
        Ownership::AppBundle => "app",
        Ownership::Owned {
            class: LeftoverClass::Regenerable,
        } => "cache",
        Ownership::Owned {
            class: LeftoverClass::DataRisk,
        } => "data",
        Ownership::ReportOnly { .. } | Ownership::Uncertain { .. } => "kept",
    }
}

/// The engine's reason for keeping an entry, if it keeps it.
pub fn kept_reason(item: &RemovalItem) -> Option<&str> {
    match &item.ownership {
        Ownership::ReportOnly { reason } | Ownership::Uncertain { reason } => Some(reason),
        Ownership::AppBundle | Ownership::Owned { .. } => None,
    }
}

/// A path without the `~/Library/` every leftover starts with; anything else
/// (the `.app`) whole.
pub fn shown_path(path: &Path, library: Option<&Path>) -> String {
    library
        .and_then(|library| path.strip_prefix(library).ok())
        .filter(|rest| !rest.as_os_str().is_empty())
        .unwrap_or(path)
        .display()
        .to_string()
}

/// Where a kept entry sits: its folder under `~/Library` (`Group Containers`).
pub fn kept_place(path: &Path, library: Option<&Path>) -> String {
    let under = library.and_then(|library| path.strip_prefix(library).ok());
    match under.and_then(|rest| rest.components().next()) {
        Some(Component::Normal(folder)) => folder.to_string_lossy().into_owned(),
        _ => shown_path(path, library),
    }
}

/// The figures of one plan under [`MENU_CONSENT`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Figures {
    pub app: u64,
    pub cache: u64,
    pub data: u64,
    /// Entries the engine only reports; never counted.
    pub kept: usize,
    /// Entries the menu's actions remove.
    pub items: usize,
    pub removable: u64,
}

pub fn figures(plan: &Plan) -> Figures {
    plan.items.iter().fold(Figures::default(), |sum, item| {
        if !item.ownership.allows(MENU_CONSENT) {
            return Figures {
                kept: sum.kept + 1,
                ..sum
            };
        }
        let size = item.size;
        let sum = Figures {
            items: sum.items + 1,
            removable: sum.removable.saturating_add(size),
            ..sum
        };
        match item.ownership {
            Ownership::AppBundle => Figures {
                app: sum.app.saturating_add(size),
                ..sum
            },
            Ownership::Owned {
                class: LeftoverClass::Regenerable,
            } => Figures {
                cache: sum.cache.saturating_add(size),
                ..sum
            },
            _ => Figures {
                data: sum.data.saturating_add(size),
                ..sum
            },
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use tiny_core::uninstall::ItemKind;

    fn item(path: &str, size: u64, ownership: Ownership) -> RemovalItem {
        RemovalItem {
            path: PathBuf::from(path),
            size,
            kind: ItemKind::Leftover,
            ownership,
        }
    }

    #[test]
    fn labels_follow_the_command_and_kept_entries_are_never_counted() {
        let owned = |class| Ownership::Owned { class };
        let plan = Plan {
            app: AppEntry {
                name: "A".into(),
                path: PathBuf::from("/Applications/A.app"),
                bundle_id: Some("com.example.a".into()),
                size: 10,
                last_used_days: None,
            },
            items: vec![
                item("/Applications/A.app", 10, Ownership::AppBundle),
                item("/L/Caches/a", 20, owned(LeftoverClass::Regenerable)),
                item("/L/Preferences/a.plist", 30, owned(LeftoverClass::DataRisk)),
                item(
                    "/L/Group Containers/g",
                    40,
                    Ownership::ReportOnly { reason: "r".into() },
                ),
                item(
                    "/L/WebKit/a",
                    50,
                    Ownership::Uncertain { reason: "s".into() },
                ),
            ],
            blocked: None,
        };
        let labels: Vec<_> = plan.items.iter().map(label).collect();
        assert_eq!(labels, ["app", "cache", "data", "kept", "kept"]);
        assert_eq!(
            figures(&plan),
            Figures {
                app: 10,
                cache: 20,
                data: 30,
                kept: 2,
                items: 3,
                removable: 60,
            }
        );
        // The menu's consent is the one that removes `data` too.
        assert_eq!(MENU_CONSENT, Consent::Explicit);
        assert!(plan.items[2].ownership.allows(MENU_CONSENT));
    }

    #[test]
    fn paths_lose_only_the_library_prefix() {
        let library = Path::new("/Users/me/Library");
        let cache = Path::new("/Users/me/Library/Caches/com.example.a");
        assert_eq!(shown_path(cache, Some(library)), "Caches/com.example.a");
        assert_eq!(
            shown_path(Path::new("/Applications/A.app"), Some(library)),
            "/Applications/A.app"
        );
        assert_eq!(shown_path(cache, None), cache.display().to_string());
        assert_eq!(
            kept_place(
                Path::new("/Users/me/Library/Group Containers/group.com.example.a"),
                Some(library)
            ),
            "Group Containers"
        );
    }

    #[test]
    fn last_use_and_refusals_read_as_the_mock() {
        assert_eq!(last_used(Some(0)), "today");
        assert_eq!(last_used(Some(2_911)), "2,911d ago");
        assert_eq!(last_used(None), "never");
        assert_eq!(
            short_refusal("system app (com.apple.Keynote) — refuse"),
            "Apple app"
        );
        assert_eq!(
            short_refusal(
                "looks like a Homebrew cask — use `brew uninstall --cask docker` (or pass --force)"
            ),
            "brew cask"
        );
        assert_eq!(short_refusal("other"), "other");
    }
}
