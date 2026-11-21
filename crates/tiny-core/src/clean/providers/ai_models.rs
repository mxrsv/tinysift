//! Local AI models that can move to the Trash, one item per model: the
//! Hugging Face download cache, one item per repository, and LM Studio's
//! model folders. Ollama's store is report-only (`ollama_models`).
//!
//! Only the model folders are items. Tokens, settings, chat history and
//! everything else beside them in the same store are never read as
//! candidates. Both categories go to the Trash in either delete mode
//! (`trash_in_every_mode`): a model imported or built on this Mac does not
//! download again.

use std::path::{Component, Path, PathBuf};

use crate::error::Result;

use super::{execute_per_item, root_as_item, CleanProvider};
use crate::clean::fs_safe::{is_dir_safe, list_children, read_entries};
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn file_name(path: &Path) -> Option<&str> {
    path.file_name().and_then(|name| name.to_str())
}

fn visible_dir(path: &Path) -> bool {
    file_name(path).is_some_and(|name| !name.is_empty() && !name.starts_with('.'))
        && is_dir_safe(path)
}

// ---------- Hugging Face ----------

pub(crate) const HUGGING_FACE_ID: &str = "huggingface";
const HUGGING_FACE_LABEL: &str = "Hugging Face models";
/// The download cache. Tokens sit beside it and are never offered.
const HUGGING_FACE_HUB: &str = ".cache/huggingface/hub";
/// One repository folder per downloaded model, dataset or space. Each holds
/// its own `blobs`, which its `snapshots` link to, so removing the folder
/// frees what it holds.
const REPO_PREFIXES: &[&str] = &["models--", "datasets--", "spaces--"];
/// A layout that keeps file contents once for every repository. A folder's
/// removal then frees nothing of what it links to, so the hub is one item.
const SHARED_BLOBS: &str = "blobs";

fn hub_dir(home: &Path) -> PathBuf {
    home.join(HUGGING_FACE_HUB)
}

pub struct HuggingFace;

impl CleanProvider for HuggingFace {
    fn id(&self) -> &'static str {
        HUGGING_FACE_ID
    }
    fn label(&self) -> &'static str {
        HUGGING_FACE_LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Models and datasets Hugging Face downloaded; they download again on next use, \
         which can be large"
            .into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    fn desktop_trash_paths(&self) -> bool {
        true
    }
    fn available(&self) -> bool {
        home().is_some_and(|h| is_dir_safe(&hub_dir(&h)))
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        Ok(home()
            .map(|h| hugging_face_items(ctx, &h))
            .unwrap_or_default())
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_per_item(items, action, HUGGING_FACE_ID)
    }
    fn check_item(&self, path: &Path) -> std::result::Result<(), String> {
        match home() {
            Some(h) if path == hub_dir(&h) || is_hub_repository(&h, path) => Ok(()),
            _ => Err("not the Hugging Face download cache or one of its repositories".into()),
        }
    }
}

/// One item per repository folder. The whole hub instead when a removal of
/// one folder could not be told apart from the others: a shared `blobs`
/// folder, or a link inside a repository that leads out of it (AM7).
fn hugging_face_items(ctx: &ScanContext<'_>, home: &Path) -> Vec<CleanItem> {
    let hub = hub_dir(home);
    if !is_dir_safe(&hub) {
        return Vec::new();
    }
    let whole_hub = || {
        root_as_item(
            ctx,
            &hub,
            HUGGING_FACE_ID,
            HUGGING_FACE_LABEL,
            RiskLevel::Review,
        )
    };
    if std::fs::symlink_metadata(hub.join(SHARED_BLOBS)).is_ok() {
        return whole_hub();
    }
    let mut repositories: Vec<PathBuf> = list_children(&hub, ctx)
        .into_iter()
        .filter(|path| is_hub_repository(home, path))
        .collect();
    repositories.sort();
    if !repositories.iter().all(|repo| links_stay_inside(repo, ctx)) {
        return whole_hub();
    }
    repositories
        .iter()
        .flat_map(|repo| {
            root_as_item(
                ctx,
                repo,
                HUGGING_FACE_ID,
                HUGGING_FACE_LABEL,
                RiskLevel::Review,
            )
        })
        .collect()
}

/// `~/.cache/huggingface/hub/<kind>--<name>`, a real folder.
fn is_hub_repository(home: &Path, path: &Path) -> bool {
    path.parent() == Some(hub_dir(home).as_path())
        && file_name(path).is_some_and(|name| {
            REPO_PREFIXES.iter().any(|prefix| {
                name.strip_prefix(prefix)
                    .is_some_and(|rest| !rest.is_empty())
            })
        })
        && is_dir_safe(path)
}

/// True when every link under `root` resolves, by its text alone, to a path
/// inside `root`. A link that cannot be read counts as leading out, and so
/// does a folder that cannot be listed.
fn links_stay_inside(root: &Path, ctx: &ScanContext<'_>) -> bool {
    let mut folders = vec![root.to_path_buf()];
    while let Some(folder) = folders.pop() {
        if std::fs::read_dir(&folder).is_err() {
            return false;
        }
        for (path, meta) in read_entries(root, &folder, ctx) {
            let kind = meta.file_type();
            if kind.is_symlink() {
                let inside = std::fs::read_link(&path)
                    .ok()
                    .and_then(|target| resolved(&path, &target))
                    .is_some_and(|target| target.starts_with(root) && target != root);
                if !inside {
                    return false;
                }
            } else if kind.is_dir() {
                folders.push(path);
            }
        }
    }
    true
}

/// Where `target`, read from the link at `link`, points, with `.` and `..`
/// applied to its text. `None` when `..` climbs above the root.
fn resolved(link: &Path, target: &Path) -> Option<PathBuf> {
    let joined = if target.is_absolute() {
        target.to_path_buf()
    } else {
        link.parent()?.join(target)
    };
    let mut out = PathBuf::new();
    for part in joined.components() {
        match part {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    Some(out)
}

// ---------- LM Studio ----------

pub(crate) const LM_STUDIO_ID: &str = "lmstudio-models";
const LM_STUDIO_LABEL: &str = "LM Studio models";
/// LM Studio's default models folder: `<publisher>/<model>/`, one folder per
/// model. A folder the user chose in LM Studio instead is not found.
const LM_STUDIO_MODELS: &str = ".lmstudio/models";

fn lm_studio_dir(home: &Path) -> PathBuf {
    home.join(LM_STUDIO_MODELS)
}

pub struct LmStudio;

impl CleanProvider for LmStudio {
    fn id(&self) -> &'static str {
        LM_STUDIO_ID
    }
    fn label(&self) -> &'static str {
        LM_STUDIO_LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Models in LM Studio's models folder; a downloaded one downloads again from LM Studio, \
         one you imported does not"
            .into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    fn desktop_trash_paths(&self) -> bool {
        true
    }
    fn available(&self) -> bool {
        home().is_some_and(|h| is_dir_safe(&lm_studio_dir(&h)))
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        Ok(home().map(|h| lm_studio_items(ctx, &h)).unwrap_or_default())
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_per_item(items, action, LM_STUDIO_ID)
    }
    fn check_item(&self, path: &Path) -> std::result::Result<(), String> {
        match home() {
            Some(h) if is_lm_studio_model(&h, path) => Ok(()),
            _ => Err("not a model folder in LM Studio's models folder".into()),
        }
    }
}

fn lm_studio_items(ctx: &ScanContext<'_>, home: &Path) -> Vec<CleanItem> {
    let root = lm_studio_dir(home);
    let mut models: Vec<PathBuf> = list_children(&root, ctx)
        .into_iter()
        .filter(|publisher| visible_dir(publisher))
        .flat_map(|publisher| list_children(&publisher, ctx))
        .filter(|model| is_lm_studio_model(home, model))
        .collect();
    models.sort();
    models
        .iter()
        .flat_map(|model| {
            root_as_item(ctx, model, LM_STUDIO_ID, LM_STUDIO_LABEL, RiskLevel::Review)
        })
        .collect()
}

/// `~/.lmstudio/models/<publisher>/<model>`, both real, visible folders.
fn is_lm_studio_model(home: &Path, path: &Path) -> bool {
    let Some(publisher) = path.parent() else {
        return false;
    };
    publisher.parent() == Some(lm_studio_dir(home).as_path())
        && visible_dir(publisher)
        && visible_dir(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::fs::symlink;

    fn paths(items: &[CleanItem]) -> Vec<PathBuf> {
        items.iter().map(|i| i.path.clone()).collect()
    }

    fn file(path: PathBuf, bytes: usize) -> PathBuf {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, vec![7u8; bytes]).unwrap();
        path
    }

    /// A repository as the hub lays it out: `blobs`, `refs`, and a snapshot
    /// linking to its own blob.
    fn repo(hub: &Path, name: &str, bytes: usize) -> PathBuf {
        let repo = hub.join(name);
        file(repo.join("blobs/abc"), bytes);
        file(repo.join("refs/main"), 8);
        fs::create_dir_all(repo.join("snapshots/1")).unwrap();
        symlink(
            "../../blobs/abc",
            repo.join("snapshots/1/model.safetensors"),
        )
        .unwrap();
        repo
    }

    fn hub_with_tokens(home: &Path) -> PathBuf {
        let hub = hub_dir(home);
        fs::create_dir_all(&hub).unwrap();
        file(home.join(".cache/huggingface/token"), 9);
        file(home.join(".cache/huggingface/stored_tokens"), 9);
        file(hub.join("version.txt"), 1);
        fs::create_dir_all(hub.join(".locks/models--org--a")).unwrap();
        hub
    }

    #[test]
    fn hugging_face_offers_one_item_per_repository() {
        let home = tempfile::tempdir().unwrap();
        let hub = hub_with_tokens(home.path());
        let a = repo(&hub, "models--org--a", 40_000);
        let b = repo(&hub, "datasets--org--b", 20_000);
        fs::create_dir_all(hub.join("models--")).unwrap();
        file(hub.join("models--org--file"), 3);
        symlink(&a, hub.join("models--org--link")).unwrap();
        let items = hugging_face_items(&ScanContext::unchecked(), home.path());
        assert_eq!(paths(&items), vec![b.clone(), a.clone()]);
        assert!(items.iter().all(|i| i.risk == RiskLevel::Review));
        assert!(items[1].size >= 40_000, "a repository frees its own blobs");
        assert!(HuggingFace.quit_apps().is_empty());
    }

    #[test]
    fn hugging_face_keeps_the_hub_whole_when_a_folder_does_not_free_what_it_links_to() {
        let home = tempfile::tempdir().unwrap();
        let hub = hub_with_tokens(home.path());
        repo(&hub, "models--org--a", 10);
        let b = repo(&hub, "models--org--b", 10);
        let shared = file(hub.join("models--org--a/blobs/shared"), 10);
        symlink(&shared, b.join("snapshots/1/shared.bin")).unwrap();
        let items = hugging_face_items(&ScanContext::unchecked(), home.path());
        assert_eq!(paths(&items), vec![hub.clone()]);

        let home = tempfile::tempdir().unwrap();
        let hub = hub_with_tokens(home.path());
        repo(&hub, "models--org--a", 10);
        file(hub.join("blobs/xyz"), 10);
        let items = hugging_face_items(&ScanContext::unchecked(), home.path());
        assert_eq!(paths(&items), vec![hub]);
    }

    #[test]
    fn a_link_climbing_out_by_dots_counts_as_leading_out() {
        let home = tempfile::tempdir().unwrap();
        let hub = hub_with_tokens(home.path());
        let a = repo(&hub, "models--org--a", 10);
        symlink("../../../models--org--x/blobs/q", a.join("snapshots/1/q")).unwrap();
        assert!(!links_stay_inside(&a, &ScanContext::unchecked()));
        assert_eq!(
            resolved(Path::new("/a/b/link"), Path::new("../../../..")),
            None
        );
        assert_eq!(
            resolved(Path::new("/a/b/link"), Path::new("./c/../d")),
            Some(PathBuf::from("/a/b/d"))
        );
    }

    #[test]
    fn lm_studio_offers_one_item_per_model_folder() {
        let home = tempfile::tempdir().unwrap();
        let root = lm_studio_dir(home.path());
        let qwen = root.join("lmstudio-community/Qwen3-8B-GGUF");
        file(qwen.join("Qwen3-8B-Q4_K_M.gguf"), 30_000);
        let gemma = root.join("google/gemma-3-4b");
        file(gemma.join("model.gguf"), 20_000);
        file(root.join("lmstudio-community/notes.txt"), 3);
        fs::create_dir_all(root.join(".internal/cache")).unwrap();
        fs::create_dir_all(root.join("google/.partial")).unwrap();
        symlink(&qwen, root.join("google/linked")).unwrap();
        file(home.path().join(".lmstudio/settings.json"), 3);
        file(home.path().join(".lmstudio/conversations/1.json"), 3);
        let items = lm_studio_items(&ScanContext::unchecked(), home.path());
        assert_eq!(paths(&items), vec![gemma.clone(), qwen.clone()]);
        assert!(items.iter().all(|i| i.risk == RiskLevel::Review));
        assert!(is_lm_studio_model(home.path(), &qwen));
        assert!(!is_lm_studio_model(home.path(), &root));
        assert!(!is_lm_studio_model(
            home.path(),
            &root.join("lmstudio-community")
        ));
        assert!(!is_lm_studio_model(
            home.path(),
            &root.join("google/linked")
        ));
    }

    /// AC-M4: nothing beside the model folders is ever an item.
    #[test]
    fn no_item_is_or_holds_a_token_settings_or_history() {
        let home = tempfile::tempdir().unwrap();
        let hub = hub_with_tokens(home.path());
        repo(&hub, "models--org--a", 10);
        let lm = lm_studio_dir(home.path());
        file(lm.join("pub/model/m.gguf"), 10);
        file(home.path().join(".lmstudio/settings.json"), 3);
        let ctx = ScanContext::unchecked();
        let mut items = hugging_face_items(&ctx, home.path());
        items.extend(lm_studio_items(&ctx, home.path()));
        assert_eq!(items.len(), 2);
        for item in &items {
            for private in [
                home.path().join(".cache/huggingface/token"),
                home.path().join(".cache/huggingface/stored_tokens"),
                home.path().join(".lmstudio/settings.json"),
                home.path().join(".lmstudio"),
                lm.clone(),
            ] {
                assert!(!private.starts_with(&item.path), "{:?}", item.path);
            }
        }
    }

    #[test]
    fn guards_refuse_paths_outside_each_pinned_shape() {
        let h = home().unwrap();
        for path in [
            PathBuf::from("/"),
            PathBuf::from("/tmp/models--org--a"),
            h.join(".cache/huggingface"),
            h.join(".cache/huggingface/token"),
            hub_dir(&h).join("blobs"),
            lm_studio_dir(&h),
            h.join(".lmstudio"),
        ] {
            assert!(HuggingFace.check_item(&path).is_err(), "{path:?}");
            assert!(LmStudio.check_item(&path).is_err(), "{path:?}");
        }
    }
}
