//! Ollama's models, listed with their size and never moved (AM6).
//!
//! A model is a manifest under `~/.ollama/models/manifests` naming blobs in
//! `~/.ollama/models/blobs`; a model built from another reuses its layers, so
//! no folder is one model. Removal belongs to `ollama rm`, which asks the
//! local server, and the server prunes unused blobs when it starts. Each item
//! is one manifest, sized by the blobs no other manifest names: what removing
//! that model alone would free.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

use crate::engine_error;
use crate::error::Result;

use super::{CleanProvider, ReportOnly};
use crate::clean::fs_safe::{is_dir_safe, read_entries};
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};

pub(crate) const ID: &str = "ollama-models";
const LABEL: &str = "Ollama models";
const MODELS: &str = ".ollama/models";
const MANIFESTS: &str = "manifests";
const BLOBS: &str = "blobs";
/// `manifests/<registry>/<namespace>/<model>/<tag>`: three folders, then the
/// manifest file.
const MANIFEST_FOLDERS: usize = 3;
const REPORT_ONLY: &str = "Ollama removes its own models: run ollama rm <model>";

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

fn models_dir(home: &Path) -> PathBuf {
    home.join(MODELS)
}

pub struct OllamaModels;

impl CleanProvider for OllamaModels {
    fn id(&self) -> &'static str {
        ID
    }
    fn label(&self) -> &'static str {
        LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Models Ollama downloaded, each sized by the layers no other model uses; ollama rm \
         removes one"
            .into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    fn desktop_report_only_reason(&self) -> ReportOnly {
        ReportOnly::NotPerPathTrash
    }
    fn available(&self) -> bool {
        home().is_some_and(|h| is_dir_safe(&models_dir(&h).join(MANIFESTS)))
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        Ok(home()
            .map(|h| ollama_items(ctx, &models_dir(&h)))
            .unwrap_or_default())
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        if matches!(action, ExecAction::EmptyTrash) {
            return Err(engine_error!("{} provider does not accept EmptyTrash", ID));
        }
        Ok(ExecReport {
            failed: items
                .iter()
                .map(|item| (item.path.clone(), REPORT_ONLY.to_string()))
                .collect(),
            ..ExecReport::default()
        })
    }
    fn check_item(&self, _path: &Path) -> std::result::Result<(), String> {
        Err(REPORT_ONLY.into())
    }
}

#[derive(serde::Deserialize)]
struct Manifest {
    config: Option<Layer>,
    #[serde(default)]
    layers: Vec<Layer>,
}

#[derive(serde::Deserialize)]
struct Layer {
    digest: String,
}

/// Every manifest file at the pinned depth, sorted.
fn manifest_files(models: &Path, ctx: &ScanContext<'_>) -> Vec<PathBuf> {
    let root = models.join(MANIFESTS);
    let mut level = vec![root.clone()];
    for _ in 0..MANIFEST_FOLDERS {
        level = level
            .iter()
            .flat_map(|dir| read_entries(&root, dir, ctx))
            .filter(|(_, meta)| meta.file_type().is_dir())
            .map(|(path, _)| path)
            .collect();
    }
    let mut files: Vec<PathBuf> = level
        .iter()
        .flat_map(|dir| read_entries(&root, dir, ctx))
        .filter(|(_, meta)| meta.file_type().is_file())
        .map(|(path, _)| path)
        .collect();
    files.sort();
    files
}

/// The blob file a digest names: `sha256:<hex>` is stored as `sha256-<hex>`.
fn blob_path(models: &Path, digest: &str) -> Option<PathBuf> {
    let (algorithm, hex) = digest.split_once(':')?;
    let valid = |part: &str| !part.is_empty() && part.chars().all(|c| c.is_ascii_alphanumeric());
    (valid(algorithm) && valid(hex)).then(|| models.join(BLOBS).join(format!("{algorithm}-{hex}")))
}

fn ollama_items(ctx: &ScanContext<'_>, models: &Path) -> Vec<CleanItem> {
    let files = manifest_files(models, ctx);
    let mut digests: Vec<(PathBuf, Vec<String>)> = Vec::new();
    // A manifest that cannot be read may name any blob, so no size can be
    // told to be a model's own.
    let mut all_known = true;
    for file in &files {
        let parsed = fs::read(file)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Manifest>(&bytes).ok());
        match parsed {
            Some(manifest) => {
                let mut names: Vec<String> = manifest
                    .config
                    .into_iter()
                    .chain(manifest.layers)
                    .map(|layer| layer.digest)
                    .collect();
                names.sort();
                names.dedup();
                digests.push((file.clone(), names));
            }
            None => all_known = false,
        }
    }
    let mut users: HashMap<&str, usize> = HashMap::new();
    for (_, names) in &digests {
        for name in names {
            *users.entry(name.as_str()).or_default() += 1;
        }
    }
    if !files.is_empty() {
        ctx.add_root(&models.join(MANIFESTS));
    }
    let mut items = Vec::new();
    for (file, names) in &digests {
        let mut size = 0u64;
        for name in names.iter().filter(|name| users[name.as_str()] == 1) {
            let Some(blob) = blob_path(models, name) else {
                continue;
            };
            match fs::symlink_metadata(&blob) {
                Ok(meta) if meta.file_type().is_file() => {
                    size = size.saturating_add(crate::space_lens::allocated(&meta));
                }
                Ok(_) => {}
                Err(error) => ctx.note_unreadable(file, &error),
            }
        }
        if !all_known {
            ctx.note_size_upper_bound(file);
        }
        items.push(CleanItem {
            category_id: ID.to_string(),
            category_label: LABEL.to_string(),
            path: file.clone(),
            size,
            risk: RiskLevel::Review,
            evidence: Vec::new(),
        });
    }
    items
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::providers::desktop_report_only;

    const BLOB: usize = 8192;

    fn blob(models: &Path, hex: &str, blocks: usize) {
        fs::create_dir_all(models.join(BLOBS)).unwrap();
        fs::write(
            models.join(BLOBS).join(format!("sha256-{hex}")),
            vec![1u8; blocks * BLOB],
        )
        .unwrap();
    }

    fn manifest(models: &Path, model: &str, tag: &str, config: &str, layers: &[&str]) -> PathBuf {
        let dir = models
            .join(MANIFESTS)
            .join("registry.ollama.ai/library")
            .join(model);
        fs::create_dir_all(&dir).unwrap();
        let layers: Vec<String> = layers
            .iter()
            .map(|hex| format!(r#"{{"mediaType":"m","digest":"sha256:{hex}","size":1}}"#))
            .collect();
        let body = format!(
            r#"{{"schemaVersion":2,"config":{{"digest":"sha256:{config}","size":1}},"layers":[{}]}}"#,
            layers.join(",")
        );
        let path = dir.join(tag);
        fs::write(&path, body).unwrap();
        path
    }

    /// AC-M1: two models sharing a layer are two items, and the shared layer
    /// counts in neither.
    #[test]
    fn each_model_is_sized_by_the_layers_only_it_uses() {
        let home = tempfile::tempdir().unwrap();
        let models = models_dir(home.path());
        blob(&models, "base", 10);
        blob(&models, "c1", 1);
        blob(&models, "c2", 1);
        blob(&models, "tuned", 4);
        let llama = manifest(&models, "llama3", "latest", "c1", &["base"]);
        let mine = manifest(&models, "mine", "v1", "c2", &["base", "tuned"]);
        let ctx = ScanContext::unchecked();
        let items = ollama_items(&ctx, &models);
        assert_eq!(
            items.iter().map(|i| &i.path).collect::<Vec<_>>(),
            [&llama, &mine]
        );
        let base = 10 * BLOB as u64;
        assert!(items[0].size < base, "the shared layer is not llama3's own");
        assert!(items[1].size >= 4 * BLOB as u64 && items[1].size < base);
        assert!(ctx.take_findings().size_upper_bound.is_empty());
    }

    #[test]
    fn an_unreadable_manifest_makes_every_size_an_upper_bound() {
        let home = tempfile::tempdir().unwrap();
        let models = models_dir(home.path());
        blob(&models, "a", 1);
        let good = manifest(&models, "a", "latest", "a", &[]);
        let bad = models
            .join(MANIFESTS)
            .join("registry.ollama.ai/library/b/latest");
        fs::create_dir_all(bad.parent().unwrap()).unwrap();
        fs::write(&bad, b"not json").unwrap();
        let ctx = ScanContext::unchecked();
        let items = ollama_items(&ctx, &models);
        assert_eq!(items.len(), 1);
        assert!(ctx.take_findings().size_upper_bound.contains(&good));
    }

    /// AC-M4 and AM6: the store's root and its blobs are never an item, and
    /// nothing is moved, from the desktop or the CLI.
    #[test]
    fn ollama_is_report_only_and_never_offers_its_store() {
        let home = tempfile::tempdir().unwrap();
        let models = models_dir(home.path());
        blob(&models, "a", 1);
        manifest(&models, "a", "latest", "a", &["a"]);
        fs::write(models.join(MANIFESTS).join("stray"), b"{}").unwrap();
        let items = ollama_items(&ScanContext::unchecked(), &models);
        assert_eq!(items.len(), 1);
        for item in &items {
            assert!(item.path.starts_with(models.join(MANIFESTS)));
            assert!(!models.starts_with(&item.path));
            assert!(!item.path.starts_with(models.join(BLOBS)));
        }
        assert_eq!(
            desktop_report_only(&OllamaModels),
            Some(ReportOnly::NotPerPathTrash)
        );
        assert!(OllamaModels.check_item(&items[0].path).is_err());
        let report = OllamaModels
            .execute(&items, ExecAction::HardDelete)
            .unwrap();
        assert_eq!(report.failed.len(), 1);
        assert!(blob_path(&models, "sha256:../x").is_none());
        assert!(blob_path(&models, "nodigest").is_none());
    }
}
