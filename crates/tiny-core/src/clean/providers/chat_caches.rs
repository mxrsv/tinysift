use std::path::PathBuf;

use crate::error::Result;

use super::{execute_per_item, root_as_item, CleanProvider};
use crate::clean::fs_safe::{is_dir_safe, list_children};
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};

const ID: &str = "chat-caches";
const LABEL: &str = "Chat app caches";

const CHAT_PATHS: &[(&str, &str)] = &[
    ("Library/Application Support/Slack/Cache", "Slack"),
    (
        "Library/Application Support/Slack/Service Worker/CacheStorage",
        "Slack",
    ),
    ("Library/Application Support/discord/Cache", "Discord"),
];

/// Telegram path uses a glob pattern: `~/Library/Group Containers/
/// *.ru.keepcoder.Telegram/account-*/postbox/media`. We resolve via
/// read_dir at discover time. Locked by "Telegram".
const TELEGRAM_GROUP_CONTAINERS: &str = "Library/Group Containers";
const TELEGRAM_APP: &str = "Telegram";

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub struct ChatCaches;

impl ChatCaches {
    pub fn new() -> Self {
        Self
    }
}

impl Default for ChatCaches {
    fn default() -> Self {
        Self::new()
    }
}

impl CleanProvider for ChatCaches {
    fn id(&self) -> &'static str {
        ID
    }
    fn label(&self) -> &'static str {
        LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Chat app caches and downloaded media the app fetches again when needed".into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    fn desktop_trash_paths(&self) -> bool {
        true
    }
    fn item_apps(&self, path: &std::path::Path) -> Vec<String> {
        let owner = home().and_then(|h| {
            if path.starts_with(h.join(TELEGRAM_GROUP_CONTAINERS)) {
                return Some(TELEGRAM_APP);
            }
            CHAT_PATHS
                .iter()
                .find(|(rel, _)| path == h.join(rel))
                .map(|(_, app)| *app)
        });
        // An unrecognised path is gated on every chat app rather than none.
        owner.map(|app| vec![app.to_string()]).unwrap_or_else(|| {
            CHAT_PATHS
                .iter()
                .map(|(_, app)| *app)
                .chain([TELEGRAM_APP])
                .map(String::from)
                .collect()
        })
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let h = match home() {
            Some(h) => h,
            None => return Ok(Vec::new()),
        };
        let mut items = Vec::new();
        for (rel, app) in CHAT_PATHS {
            let path = h.join(rel);
            if !is_dir_safe(&path) {
                continue;
            }
            if ctx.known_running(app) {
                continue;
            }
            items.extend(root_as_item(ctx, &path, ID, LABEL, RiskLevel::Review));
        }
        // Telegram: glob resolution.
        let telegram_running = ctx.known_running(TELEGRAM_APP);
        for tg_media in telegram_media_dirs(ctx, &h.join(TELEGRAM_GROUP_CONTAINERS)) {
            if telegram_running {
                continue;
            }
            items.extend(root_as_item(ctx, &tg_media, ID, LABEL, RiskLevel::Review));
        }
        Ok(items)
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_per_item(items, action, ID)
    }
}

/// Resolves `<group_containers>/*.ru.keepcoder.Telegram/account-*/postbox/
/// media` for every matching account.
pub fn telegram_media_dirs(
    ctx: &ScanContext<'_>,
    group_containers: &std::path::Path,
) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for path in list_children(group_containers, ctx) {
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        if !name.ends_with(".ru.keepcoder.Telegram") {
            continue;
        }
        for acc_path in list_children(&path, ctx) {
            let acc_name = match acc_path.file_name().and_then(|n| n.to_str()) {
                Some(n) => n,
                None => continue,
            };
            if !acc_name.starts_with("account-") {
                continue;
            }
            let media = acc_path.join("postbox/media");
            if is_dir_safe(&media) {
                out.push(media);
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::process::test_support::MockChecker;
    use crate::clean::providers::known_category_ids;
    use std::fs;

    fn tempdir(label: &str) -> PathBuf {
        let mut base = std::env::temp_dir();
        base.push(format!(
            "tiny-clean-chat-{}-{}",
            label,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn chat_id_in_known_categories() {
        assert!(known_category_ids().contains(&ID));
    }

    #[test]
    fn chat_paths_skip_running_apps() {
        let mock = MockChecker::with_running(["Slack", "Discord", "Telegram"]);
        let ctx = ScanContext::new(None, &mock);
        let items = ChatCaches::new().discover(&ctx).unwrap();
        for item in &items {
            let s = item.path.to_string_lossy();
            assert!(!s.contains("/Slack/"), "Slack path leaked: {}", s);
            assert!(!s.contains("/discord/"), "Discord path leaked: {}", s);
            assert!(
                !s.contains(".ru.keepcoder.Telegram"),
                "Telegram path leaked: {}",
                s
            );
        }
    }

    #[test]
    fn telegram_media_dirs_resolves_account_glob() {
        let root = tempdir("tg");
        let group = root
            .join("6N38VWS5BX.ru.keepcoder.Telegram")
            .join("account-12345");
        fs::create_dir_all(group.join("postbox/media")).unwrap();
        // Wrong-suffix container → must skip.
        fs::create_dir_all(root.join("other.app/account-1/postbox/media")).unwrap();
        // Wrong-prefix account → must skip.
        fs::create_dir_all(root.join("X.ru.keepcoder.Telegram/profile-1/postbox/media")).unwrap();
        let found = telegram_media_dirs(&ScanContext::unchecked(), &root);
        assert_eq!(found.len(), 1);
        assert!(found[0].ends_with("account-12345/postbox/media"));
        let _ = crate::clean::fs_safe::remove_recursive_safe(&root);
    }
}
