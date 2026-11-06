//! Build folders inside projects, beside the manifest that proves they are
//! generated: `.next` (Next.js, `package.json`), `Pods` (CocoaPods,
//! `Podfile`), `build` (Gradle's `build.gradle[.kts]`, or a `package.json`
//! that names it as its output) and `dist` (a `package.json` or
//! `tsconfig.json` that names it as its output). A folder with the name but
//! no such proof is not offered: `build/` and `dist/` hold sources in many
//! projects. Activity and git are judged as for `node-modules` and
//! `rust-targets` ([`assess_build_output`]), and a folder holding a file git
//! tracks is never offered.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::Result;

use super::project_activity::{assess_build_output, refuse_tracked, BUILD_OUTPUT_SAFE_DAYS};
use super::project_walk::{project_walk, Walker};
use super::{dev_search_roots, execute_per_item, is_idle, CleanProvider};
use crate::clean::fs_safe::dir_size_checked;
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};
use crate::options::CleanOptions;
use crate::runner::CommandRunner;

const ID: &str = "build-folders";
const LABEL: &str = "Build folders (idle)";
/// The desktop lists every project, idle or not.
const ALL_PROJECTS_LABEL: &str = "Build folders";
const PACKAGE_JSON: &str = "package.json";
const TSCONFIG: &str = "tsconfig.json";
const GRADLE: &[&str] = &["build.gradle", "build.gradle.kts"];
/// `package.json` fields that name a package's built files.
const OUTPUT_FIELDS: &[&str] = &[
    "main", "module", "types", "typings", "browser", "bin", "exports", "files",
];

pub struct BuildFolders {
    pub idle_days: u64,
    /// List every project's build folders (the desktop), not only idle ones.
    pub all_projects: bool,
    pub search_roots: Vec<PathBuf>,
    runner: Arc<dyn CommandRunner>,
}

impl BuildFolders {
    /// The provider `opts` asks for: every project when `all_build_output`.
    pub fn with_options(opts: &CleanOptions, runner: Arc<dyn CommandRunner>) -> Self {
        Self {
            idle_days: opts.idle_days,
            all_projects: opts.all_build_output,
            search_roots: dev_search_roots(),
            runner,
        }
    }

    /// `None` lists every project.
    fn idle_filter(&self) -> Option<u64> {
        (!self.all_projects).then_some(self.idle_days)
    }
}

impl CleanProvider for BuildFolders {
    fn id(&self) -> &'static str {
        ID
    }
    fn label(&self) -> &'static str {
        match self.all_projects {
            true => ALL_PROJECTS_LABEL,
            false => LABEL,
        }
    }
    fn inclusion_reason(&self) -> String {
        let folders =
            ".next, Pods, and build or dist folders their project's manifest names as output";
        if self.all_projects {
            return format!("{folders}; building the project restores them. Safe once a project in git has no edits, commits or uncommitted changes for {BUILD_OUTPUT_SAFE_DAYS} days");
        }
        format!("{folders}, in projects with no edits, commits or uncommitted changes for {} days; building the project restores them", self.idle_days)
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    fn desktop_trash_paths(&self) -> bool {
        true
    }
    fn available(&self) -> bool {
        !self.search_roots.is_empty()
    }
    /// Still proven build output, and holding no file git tracks.
    fn check_item(&self, path: &Path) -> std::result::Result<(), String> {
        if manifest_of(path).is_none() {
            return Err("its project no longer names it as build output".into());
        }
        refuse_tracked(&*self.runner, path)
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let mut items = Vec::new();
        for root in &self.search_roots {
            for (found, manifest) in find_build_folders(ctx, root, self.idle_filter()) {
                if ctx.is_cancelled() {
                    return Ok(Vec::new());
                }
                let Some((risk, evidence)) =
                    assess_build_output(&*self.runner, &found, &manifest, self.idle_filter(), ctx)?
                else {
                    continue;
                };
                let size = dir_size_checked(&found, ctx);
                items.push(CleanItem {
                    category_id: ID.to_string(),
                    category_label: self.label().to_string(),
                    path: found,
                    size,
                    risk,
                    evidence,
                });
            }
        }
        Ok(items)
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_per_item(items, action, ID)
    }
}

/// Walks `root` symlink-safe for proven build folders and their manifests,
/// the manifest modified more than `idle_days` ago unless that is `None`.
/// Never descends into a found folder, `node_modules`, a Rust `target`
/// (`rust-targets` lists it) or a hidden folder.
/// The rule lives in [`project_walk`]'s `Walker::BuildFolders`.
pub fn find_build_folders(
    ctx: &ScanContext<'_>,
    root: &Path,
    idle_days: Option<u64>,
) -> Vec<(PathBuf, PathBuf)> {
    project_walk(ctx, root, Walker::BuildFolders)
        .into_iter()
        .filter_map(|path| manifest_of(&path).map(|manifest| (path, manifest)))
        .filter(|(_, manifest)| idle_days.is_none_or(|days| is_idle(manifest, days)))
        .collect()
}

/// The manifest that proves `folder` is build output, or `None`.
pub(crate) fn manifest_of(folder: &Path) -> Option<PathBuf> {
    let name = folder.file_name()?.to_str()?;
    let project = folder.parent()?;
    let file = |name: &str| Some(project.join(name)).filter(|p| p.is_file());
    match name {
        ".next" => file(PACKAGE_JSON),
        "Pods" => file("Podfile"),
        "build" => GRADLE
            .iter()
            .find_map(|gradle| file(gradle))
            .or_else(|| file(PACKAGE_JSON).filter(|p| package_names(p, name))),
        "dist" => file(PACKAGE_JSON)
            .filter(|p| package_names(p, name) || tsconfig_names(&project.join(TSCONFIG), name)),
        _ => None,
    }
}

/// `package.json` names `folder` in one of its output fields.
fn package_names(package: &Path, folder: &str) -> bool {
    let Some(json) = read_json(package) else {
        return false;
    };
    OUTPUT_FIELDS
        .iter()
        .filter_map(|field| json.get(field))
        .any(|value| names_folder(value, folder))
}

/// `tsconfig.json` writes its output to `folder` (`compilerOptions.outDir`).
/// A tsconfig with comments is not JSON and proves nothing.
fn tsconfig_names(tsconfig: &Path, folder: &str) -> bool {
    read_json(tsconfig)
        .as_ref()
        .and_then(|json| {
            json.get("compilerOptions")?
                .get("outDir")?
                .as_str()
                .map(str::to_owned)
        })
        .is_some_and(|out| first_component(&out) == Some(folder))
}

fn read_json(path: &Path) -> Option<serde_json::Value> {
    let text = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&text).ok()
}

/// A string inside `value` (nested arrays and objects too) is a path whose
/// first component is `folder`.
fn names_folder(value: &serde_json::Value, folder: &str) -> bool {
    match value {
        serde_json::Value::String(path) => first_component(path) == Some(folder),
        serde_json::Value::Array(values) => values.iter().any(|v| names_folder(v, folder)),
        serde_json::Value::Object(map) => map.values().any(|v| names_folder(v, folder)),
        _ => false,
    }
}

/// `./dist/index.js` and `dist` → `dist`; nothing for `../dist` or `/dist`.
fn first_component(path: &str) -> Option<&str> {
    let mut parts = path.split('/').filter(|p| !p.is_empty() && *p != ".");
    let first = parts.next()?;
    (!path.starts_with('/') && first != "..").then_some(first)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn project(root: &Path, name: &str, files: &[(&str, &str)], output: &[&str]) -> PathBuf {
        let dir = root.join(name);
        fs::create_dir_all(&dir).unwrap();
        for (file, text) in files {
            fs::write(dir.join(file), text).unwrap();
        }
        for folder in output {
            fs::create_dir_all(dir.join(folder)).unwrap();
            fs::write(dir.join(folder).join("out"), vec![1u8; 4096]).unwrap();
        }
        dir
    }

    fn found(root: &Path) -> Vec<String> {
        let mut names: Vec<String> = find_build_folders(&ScanContext::unchecked(), root, None)
            .into_iter()
            .map(|(path, _)| path.strip_prefix(root).unwrap().display().to_string())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn only_folders_their_manifest_proves_generated_are_found() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        project(root, "next", &[(PACKAGE_JSON, "{}")], &[".next"]);
        project(root, "pods", &[("Podfile", "")], &["Pods"]);
        project(root, "gradle", &[("build.gradle.kts", "")], &["build"]);
        let lib = r#"{"main": "./dist/index.js", "files": ["dist"]}"#;
        project(root, "lib", &[(PACKAGE_JSON, lib)], &["dist"]);
        let ts = r#"{"compilerOptions": {"outDir": "./dist"}}"#;
        project(
            root,
            "ts",
            &[(PACKAGE_JSON, "{}"), (TSCONFIG, ts)],
            &["dist"],
        );
        let cra = r#"{"exports": {".": {"import": "./build/x.mjs"}}}"#;
        project(root, "cra", &[(PACKAGE_JSON, cra)], &["build"]);
        // The name alone proves nothing: these hold sources.
        project(
            root,
            "scripts",
            &[(PACKAGE_JSON, r#"{"main": "src/index.js"}"#)],
            &["build", "dist"],
        );
        project(root, "plain", &[], &["build", "dist", ".next", "Pods"]);
        project(
            root,
            "outside",
            &[(PACKAGE_JSON, r#"{"main": "../dist/x.js"}"#)],
            &["dist"],
        );
        project(
            root,
            "jsonc",
            &[(PACKAGE_JSON, "{}"), (TSCONFIG, "{ // c\n }")],
            &["dist"],
        );
        assert_eq!(
            found(root),
            [
                "cra/build",
                "gradle/build",
                "lib/dist",
                "next/.next",
                "pods/Pods",
                "ts/dist"
            ]
        );
    }

    #[test]
    fn the_walk_skips_node_modules_rust_targets_hidden_and_found_folders() {
        let root = tempfile::tempdir().unwrap();
        let root = root.path();
        let pkg = r#"{"main": "dist/i.js"}"#;
        let app = project(root, "app", &[(PACKAGE_JSON, "{}")], &[".next"]);
        project(
            &app.join(".next"),
            "inner",
            &[(PACKAGE_JSON, pkg)],
            &["dist"],
        );
        project(
            &app.join("node_modules"),
            "dep",
            &[(PACKAGE_JSON, pkg)],
            &["dist"],
        );
        let crate_dir = project(root, "crate", &[("Cargo.toml", "")], &["target"]);
        project(
            &crate_dir.join("target"),
            "debug",
            &[("build.gradle", "")],
            &["build"],
        );
        project(
            &root.join(".hidden"),
            "x",
            &[(PACKAGE_JSON, pkg)],
            &["dist"],
        );
        assert_eq!(found(root), ["app/.next"]);
    }

    #[test]
    fn the_cli_offers_only_idle_projects() {
        let root = tempfile::tempdir().unwrap();
        let dir = project(root.path(), "next", &[(PACKAGE_JSON, "{}")], &[".next"]);
        assert!(find_build_folders(&ScanContext::unchecked(), root.path(), Some(30)).is_empty());
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(31 * 86_400);
        fs::File::open(dir.join(PACKAGE_JSON))
            .unwrap()
            .set_modified(old)
            .unwrap();
        let idle = find_build_folders(&ScanContext::unchecked(), root.path(), Some(30));
        assert_eq!(idle, [(dir.join(".next"), dir.join(PACKAGE_JSON))]);
    }

    #[test]
    fn the_guard_refuses_a_folder_its_project_no_longer_names() {
        let root = tempfile::tempdir().unwrap();
        let dir = project(
            root.path(),
            "lib",
            &[(PACKAGE_JSON, r#"{"main": "dist/i.js"}"#)],
            &["dist"],
        );
        let runner = Arc::new(crate::runner::test_support::MockRunner::new());
        let provider = BuildFolders {
            idle_days: 30,
            all_projects: true,
            search_roots: vec![root.path().to_path_buf()],
            runner,
        };
        assert_eq!(provider.check_item(&dir.join("dist")), Ok(()));
        fs::write(dir.join(PACKAGE_JSON), r#"{"main": "src/i.js"}"#).unwrap();
        assert!(provider.check_item(&dir.join("dist")).is_err());
    }
}
