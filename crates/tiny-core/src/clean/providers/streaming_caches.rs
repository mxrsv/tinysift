use std::path::PathBuf;

use crate::error::Result;

use super::{execute_per_item, root_as_item, CleanProvider};
use crate::clean::fs_safe::is_dir_safe;
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};

const ID: &str = "streaming-caches";
const LABEL: &str = "Streaming app caches";

/// (rel_path_under_home, app_name_locking_it). Skip path when its app is
/// running — Spotify especially keeps PersistentCache locked while open.
const STREAMING_PATHS: &[(&str, &str)] = &[
    ("Library/Caches/com.spotify.client", "Spotify"),
    (
        "Library/Application Support/Spotify/PersistentCache",
        "Spotify",
    ),
    (
        "Library/Containers/com.netflix.Netflix/Data/Library/Caches",
        "Netflix",
    ),
    (
        "Library/Containers/com.apple.Music/Data/Library/Caches",
        "Music",
    ),
];

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub struct StreamingCaches;

impl StreamingCaches {
    pub fn new() -> Self {
        Self
    }
}

impl Default for StreamingCaches {
    fn default() -> Self {
        Self::new()
    }
}

impl CleanProvider for StreamingCaches {
    fn id(&self) -> &'static str {
        ID
    }
    fn label(&self) -> &'static str {
        LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Streaming app caches the app downloads again when needed".into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    fn desktop_trash_paths(&self) -> bool {
        true
    }
    fn item_apps(&self, path: &std::path::Path) -> Vec<String> {
        let owner = home().and_then(|h| {
            STREAMING_PATHS
                .iter()
                .find(|(rel, _)| path == h.join(rel))
                .map(|(_, app)| *app)
        });
        // An unrecognised path is gated on every streaming app rather than none.
        owner.map(|app| vec![app.to_string()]).unwrap_or_else(|| {
            STREAMING_PATHS
                .iter()
                .map(|(_, app)| app.to_string())
                .collect()
        })
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let h = match home() {
            Some(h) => h,
            None => return Ok(Vec::new()),
        };
        let mut items = Vec::new();
        for (rel, app) in STREAMING_PATHS {
            let path = h.join(rel);
            if !is_dir_safe(&path) {
                continue;
            }
            if ctx.known_running(app) {
                continue;
            }
            items.extend(root_as_item(ctx, &path, ID, LABEL, RiskLevel::Review));
        }
        Ok(items)
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_per_item(items, action, ID)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::process::test_support::MockChecker;
    use crate::clean::providers::known_category_ids;

    #[test]
    fn streaming_id_in_known_categories() {
        assert!(known_category_ids().contains(&ID));
    }

    #[test]
    fn streaming_paths_filtered_by_running_app() {
        // No paths exist on this machine necessarily, but we can prove
        // the gate logic by injecting a checker. Build minimum that we
        // can: assert that with Spotify running, discover() never
        // includes a Spotify-tagged CleanItem (size could be 0 if dir
        // missing, but the path string itself must not appear).
        let mock = MockChecker::with_running(["Spotify"]);
        let ctx = ScanContext::new(None, &mock);
        let items = StreamingCaches::new().discover(&ctx).unwrap();
        for item in &items {
            let s = item.path.to_string_lossy();
            assert!(
                !s.contains("com.spotify.client") && !s.contains("Spotify/PersistentCache"),
                "Spotify path leaked while Spotify running: {}",
                s
            );
        }
    }
}
