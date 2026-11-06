//! One walk of a dev search root for every project provider: `node_modules`
//! (`node-modules`), Rust `target/` (`rust-targets`), `__pycache__` and
//! virtualenvs (`python-caches`) and build folders (`build-folders`). Each
//! used to walk the roots itself, five walks of the same tree per scan.
//!
//! Every walker keeps its own rule ([`Walker::visit`]): what it records and
//! where it stops. The walk descends into a folder while at least one walker
//! still wants it, and tracks which ones do, so each walker sees exactly the
//! folders, in exactly the order, its own walk saw. What a read could not
//! reach is recorded per walker the same way, and handed to each provider's
//! context when it takes its share ([`project_walk`]). The walk is kept in
//! the scan's [`ScanContext`] and runs once per root per scan.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use super::build_folders::manifest_of;
use super::is_node_modules;
use super::python_caches::{is_named_venv, python_manifest};
use crate::clean::fs_safe::read_entries;
use crate::clean::scan_context::{ScanContext, ScanFindings};

/// The project walkers, one bit each in a folder's set of active walkers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Walker {
    NodeModules,
    RustTargets,
    Pycache,
    Venvs,
    BuildFolders,
}

const WALKERS: [Walker; 5] = [
    Walker::NodeModules,
    Walker::RustTargets,
    Walker::Pycache,
    Walker::Venvs,
    Walker::BuildFolders,
];
const ALL: u8 = (1 << WALKERS.len()) - 1;

impl Walker {
    fn bit(self) -> u8 {
        1 << self as u8
    }

    /// Whether this walker records the folder `path` and whether it goes on
    /// into it, as its own walk did. Unidle candidates are recorded too:
    /// each provider applies its idle rule to what it takes.
    fn visit(self, path: &Path, root: &Path) -> (bool, bool) {
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let file_beside = |file: &str| path.with_file_name(file).is_file();
        match self {
            Walker::NodeModules if name == "node_modules" => (file_beside("package.json"), false),
            Walker::NodeModules => (false, true),
            _ if is_node_modules(path) => (false, false),
            Walker::RustTargets if name == "target" => (file_beside("Cargo.toml"), false),
            Walker::RustTargets => (false, true),
            Walker::Pycache if is_named_venv(path) => (false, false),
            Walker::Pycache if name == "__pycache__" => (true, false),
            Walker::Pycache => (false, true),
            Walker::Venvs if is_named_venv(path) => {
                let manifest = path.parent().and_then(python_manifest);
                (manifest.is_some(), false)
            }
            Walker::Venvs => (false, true),
            Walker::BuildFolders if manifest_of(path).is_some() => (true, false),
            Walker::BuildFolders => {
                let rust_target = name == "target" && file_beside("Cargo.toml");
                (
                    false,
                    path == root || !(name.starts_with('.') || rust_target),
                )
            }
        }
    }
}

/// What one walk of a root found, per walker.
#[derive(Debug, Default)]
pub(crate) struct ProjectWalk {
    found: [Vec<PathBuf>; WALKERS.len()],
    findings: [ScanFindings; WALKERS.len()],
}

impl ProjectWalk {
    fn walk(root: &Path, ctx: &ScanContext<'_>) -> Self {
        let mut walk = Self::default();
        let local = ScanContext::worker(ctx.cancel_flag());
        let meta = match std::fs::symlink_metadata(root) {
            Ok(meta) => meta,
            Err(e) => {
                local.note_unreadable(root, &e);
                walk.attribute(ALL, local.take_findings());
                return walk;
            }
        };
        if meta.file_type().is_symlink() || !meta.file_type().is_dir() {
            return walk;
        }
        local.add_root(root);
        walk.attribute(ALL, local.take_findings());
        let active = walk.visit(ALL, root, root);
        if active != 0 {
            walk.descend(root, root, active, ctx);
        }
        walk
    }

    /// Records `path` for the walkers of `active` that take it and returns
    /// the walkers that go on into it.
    fn visit(&mut self, active: u8, path: &Path, root: &Path) -> u8 {
        let mut descend = 0;
        for walker in WALKERS.into_iter().filter(|w| active & w.bit() != 0) {
            let (record, go_on) = walker.visit(path, root);
            if record {
                self.found[walker as usize].push(path.to_path_buf());
            }
            if go_on {
                descend |= walker.bit();
            }
        }
        descend
    }

    fn descend(&mut self, root: &Path, dir: &Path, active: u8, ctx: &ScanContext<'_>) {
        let local = ScanContext::worker(ctx.cancel_flag());
        let entries = read_entries(root, dir, &local);
        self.attribute(active, local.take_findings());
        for (path, meta) in entries {
            if ctx.is_cancelled() {
                return;
            }
            if meta.file_type().is_symlink() || !meta.file_type().is_dir() {
                continue;
            }
            let next = self.visit(active, &path, root);
            if next != 0 {
                self.descend(root, &path, next, ctx);
            }
        }
    }

    /// What a read recorded, for every walker that made it.
    fn attribute(&mut self, active: u8, findings: ScanFindings) {
        for walker in WALKERS.into_iter().filter(|w| active & w.bit() != 0) {
            self.findings[walker as usize].absorb(findings.clone());
        }
    }
}

/// `walker`'s share of the walk of `root`, shared by every provider in the
/// scan of `ctx`: what it found, in its own walk's order, with what its walk
/// recorded (roots, unreadable entries) handed to `ctx`.
pub(crate) fn project_walk(ctx: &ScanContext<'_>, root: &Path, walker: Walker) -> Vec<PathBuf> {
    let walk = ctx.project_walk(root, |ctx| Arc::new(ProjectWalk::walk(root, ctx)));
    ctx.absorb(walk.findings[walker as usize].clone());
    walk.found[walker as usize].clone()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn tree(root: &Path) {
        for dir in [
            "web/node_modules/dep/node_modules",
            "web/.next",
            "web/src/__pycache__",
            "crate/target/debug/build",
            "py/.venv/lib/__pycache__",
            "py/pkg/__pycache__",
            ".hidden/app/node_modules",
            ".hidden/lib/dist",
        ] {
            fs::create_dir_all(root.join(dir)).unwrap();
        }
        for (file, text) in [
            ("web/package.json", "{}"),
            ("crate/Cargo.toml", ""),
            ("crate/target/debug/build.gradle", ""),
            ("py/pyproject.toml", ""),
            ("py/.venv/pyvenv.cfg", ""),
            (".hidden/app/package.json", "{}"),
            (".hidden/lib/package.json", r#"{"main": "dist/i.js"}"#),
        ] {
            fs::write(root.join(file), text).unwrap();
        }
    }

    fn names(root: &Path, found: Vec<PathBuf>) -> Vec<String> {
        found
            .iter()
            .map(|p| p.strip_prefix(root).unwrap().display().to_string())
            .collect()
    }

    #[test]
    fn one_walk_gives_each_walker_what_its_own_rule_finds() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        tree(root);
        let ctx = ScanContext::unchecked();
        let mut node = names(root, project_walk(&ctx, root, Walker::NodeModules));
        node.sort();
        assert_eq!(
            node,
            [".hidden/app/node_modules", "web/node_modules"],
            "never inside one"
        );
        assert_eq!(
            names(root, project_walk(&ctx, root, Walker::RustTargets)),
            ["crate/target"]
        );
        let mut pycache = names(root, project_walk(&ctx, root, Walker::Pycache));
        pycache.sort();
        assert_eq!(
            pycache,
            ["py/pkg/__pycache__", "web/src/__pycache__"],
            "never inside a venv"
        );
        assert_eq!(
            names(root, project_walk(&ctx, root, Walker::Venvs)),
            ["py/.venv"]
        );
        assert_eq!(
            names(root, project_walk(&ctx, root, Walker::BuildFolders)),
            ["web/.next"],
            "never in a hidden folder or a Rust target"
        );
        assert!(
            ctx.take_findings().roots.contains(root),
            "each share adds the root"
        );
    }

    #[test]
    fn the_root_is_walked_once_per_scan() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        tree(root);
        let ctx = ScanContext::unchecked();
        assert_eq!(project_walk(&ctx, root, Walker::RustTargets).len(), 1);
        fs::create_dir_all(root.join("later/target")).unwrap();
        fs::write(root.join("later/Cargo.toml"), "").unwrap();
        assert_eq!(
            project_walk(&ctx, root, Walker::RustTargets).len(),
            1,
            "the scan's walk"
        );
        let next_scan = ScanContext::unchecked();
        assert_eq!(project_walk(&next_scan, root, Walker::RustTargets).len(), 2);
    }

    #[test]
    fn an_unreadable_folder_counts_only_for_the_walkers_that_read_it() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let hidden = root.join(".locked");
        fs::create_dir_all(&hidden).unwrap();
        fs::set_permissions(&hidden, fs::Permissions::from_mode(0o000)).unwrap();
        let ctx = ScanContext::unchecked();
        project_walk(&ctx, root, Walker::BuildFolders);
        let build = ctx.take_findings();
        project_walk(&ctx, root, Walker::RustTargets);
        let rust = ctx.take_findings();
        fs::set_permissions(&hidden, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(
            build.unreadable.is_empty(),
            "build folders never enter a hidden folder"
        );
        assert_eq!(rust.unreadable[root].entries, 1, "Rust targets do");
    }
}
