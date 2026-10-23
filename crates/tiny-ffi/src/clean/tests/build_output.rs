//! Project build output on the desktop (spec OC15–OC17): its own section,
//! every project listed, Safe or Review per candidate, folders git cannot
//! check refused with a reason, and the consent contract unchanged. Temp
//! dirs, fake runners and `FakeTrash` only.

use std::sync::Arc;

use tiny_core::clean::providers::{node_modules::NodeModules, rust_targets::RustTargets};
use tiny_core::runner::{CommandError, CommandOutcome, CommandOutput};

use super::*;

/// The fixtures sit outside any git work tree, so no tool may run.
struct NoTools;

impl CommandRunner for NoTools {
    fn which(&self, bin: &str) -> bool {
        panic!("{bin} must not run")
    }
    fn run(&self, bin: &str, _: &[&str]) -> CommandOutput {
        panic!("{bin} must not run")
    }
    fn output(&self, bin: &str, _: &[&str], _: Duration) -> Result<CommandOutcome, CommandError> {
        panic!("{bin} must not run")
    }
}

/// `/usr/bin/git` for fixtures under a `.git` folder: every project's last
/// commit is 30 days old, every work tree clean, and `ls-files` answers
/// `ls_files`.
struct Git {
    ls_files: CommandOutcome,
}

impl Git {
    /// `ls-files` lists nothing: no tracked file inside the build output.
    fn untracked() -> Self {
        Self {
            ls_files: answer(String::new()),
        }
    }
}

fn answer(stdout: String) -> CommandOutcome {
    CommandOutcome {
        status: Some(0),
        stdout,
        stderr: String::new(),
    }
}

impl CommandRunner for Git {
    fn which(&self, bin: &str) -> bool {
        panic!("{bin} must not be looked up")
    }
    fn run(&self, bin: &str, _: &[&str]) -> CommandOutput {
        panic!("{bin} must not run unbounded")
    }
    fn output(
        &self,
        bin: &str,
        args: &[&str],
        _: Duration,
    ) -> Result<CommandOutcome, CommandError> {
        assert_eq!(bin, "/usr/bin/git");
        // `--no-optional-locks -C <dir> <subcommand> ...`
        Ok(match args.get(3) {
            Some(&"log") => {
                let now = SystemTime::now().duration_since(SystemTime::UNIX_EPOCH);
                answer(format!("{}\n", now.unwrap().as_secs() - 30 * 86_400))
            }
            Some(&"status") => answer(String::new()),
            Some(&"ls-files") => self.ls_files.clone(),
            other => panic!("unexpected git {other:?}"),
        })
    }
}

#[test]
fn the_desktop_option_reaches_core_options() {
    let options = |all_build_output| FfiCleanOptions {
        categories: Vec::new(),
        include_review: true,
        include_destructive: false,
        idle_days: 30,
        all_build_output,
        include_trash: false,
        tool_commands: false,
    };
    assert!(CleanOptions::from(options(true)).all_build_output);
    assert!(!CleanOptions::from(options(false)).all_build_output);
}

#[test]
fn every_category_carries_its_desktop_section() {
    let dir = fixture_dir("sections", &["x"], 1);
    let providers: Vec<Box<dyn CleanProvider>> =
        ["node-modules", "user-logs", "downloads-old", "logs"]
            .into_iter()
            .map(|id| Box::new(Fixture::new(id, &dir)) as Box<dyn CleanProvider>)
            .collect();
    let discovery = discover(&TestSession::new(), &providers).unwrap();
    let sections: Vec<_> = discovery
        .categories
        .iter()
        .map(|c| (c.id.as_str(), c.section))
        .collect();
    assert_eq!(
        sections,
        [
            ("node-modules", Some(FfiCleanSection::BuildOutput)),
            ("user-logs", Some(FfiCleanSection::Rebuilt)),
            ("downloads-old", Some(FfiCleanSection::YourFiles)),
            ("logs", None),
        ]
    );
    remove(&dir);
}

/// `<root>/<name>/<manifest>`, `days` old, beside `<output>/file`.
fn project(root: &Path, name: &str, manifest: &str, output: &str, days: u64) -> PathBuf {
    let dir = root.join(name);
    std::fs::create_dir_all(dir.join(output)).unwrap();
    std::fs::write(dir.join(output).join("file"), b"x").unwrap();
    let manifest = dir.join(manifest);
    std::fs::write(&manifest, b"{}").unwrap();
    let when = SystemTime::now() - Duration::from_secs(days * 86_400);
    std::fs::File::open(&manifest)
        .unwrap()
        .set_modified(when)
        .unwrap();
    dir.join(output)
}

/// The build output providers for `options`, searching `root`.
fn build_output(
    options: &CleanOptions,
    root: &Path,
    runner: Arc<dyn CommandRunner>,
) -> Vec<Box<dyn CleanProvider>> {
    let mut node_modules = NodeModules::with_options(options, runner.clone());
    node_modules.search_roots = vec![root.to_path_buf()];
    let mut rust_targets = RustTargets::with_options(options, runner);
    rust_targets.search_roots = vec![root.to_path_buf()];
    vec![Box::new(node_modules), Box::new(rust_targets)]
}

fn candidate<'a>(discovery: &'a FfiDiscovery, path: &Path) -> &'a FfiCleanCandidate {
    let path = path.to_string_lossy();
    discovery
        .categories
        .iter()
        .flat_map(|c| &c.candidates)
        .find(|c| c.path == path)
        .unwrap_or_else(|| panic!("{path} not listed"))
}

fn risk_of(discovery: &FfiDiscovery, path: &Path) -> FfiRisk {
    candidate(discovery, path).risk
}

/// The desktop's scan of `root`'s build output, with `runner` as git.
fn discover_build_output(
    session: &TinySession,
    root: &Path,
    runner: Arc<dyn CommandRunner>,
) -> FfiDiscovery {
    let options = desktop_options();
    session
        .discover_clean(
            options.clone(),
            &build_output(&options, root, runner),
            &NotRunning,
            &CancellationToken::default(),
            &Progress::default(),
        )
        .unwrap()
}

fn desktop_options() -> CleanOptions {
    CleanOptions {
        all_build_output: true,
        ..CleanOptions::default()
    }
}

#[test]
fn idle_build_output_outside_git_is_listed_for_review_with_the_reason() {
    let root = fixture_dir("outside-git", &[], 0);
    let loose = project(&root, "loose", "Cargo.toml", "target", 40);
    let discovery = discover_build_output(&TestSession::new(), &root, Arc::new(NoTools));
    let candidate = candidate(&discovery, &loose);
    assert_eq!(candidate.risk, FfiRisk::Review);
    assert!(matches!(
        candidate.evidence.as_slice(),
        [
            FfiEvidence::ManifestModified { .. },
            FfiEvidence::NotGitRepo
        ]
    ));
    remove(&root);
}

#[test]
fn build_output_git_cannot_check_is_never_offered_and_the_category_says_why() {
    let root = fixture_dir("git-shim", &[], 0);
    std::fs::create_dir(root.join(".git")).unwrap();
    let target = project(&root, "crate", "Cargo.toml", "target", 40);
    let shim = Git {
        ls_files: CommandOutcome {
            status: Some(1),
            stdout: String::new(),
            stderr: "xcrun: error: invalid active developer path \
                     (/Library/Developer/CommandLineTools)\n"
                .into(),
        },
    };
    let discovery = discover_build_output(&TestSession::new(), &root, Arc::new(shim));
    let category = discovery
        .categories
        .iter()
        .find(|c| c.id == "rust-targets")
        .unwrap();
    assert_eq!(category.status, FfiCategoryStatus::Found);
    assert!(category.candidates.is_empty());
    assert_eq!(category.refused.len(), 1);
    let refused = &category.refused[0];
    assert_eq!(refused.path, target.to_string_lossy());
    assert!(
        refused
            .reason
            .starts_with("git could not tell whether it holds tracked files"),
        "{}",
        refused.reason
    );
    assert!(
        refused.reason.contains("invalid active developer path"),
        "{}",
        refused.reason
    );
    remove(&root);
}

#[test]
fn safe_build_output_moves_without_review_consent_and_review_output_still_needs_it() {
    let root = fixture_dir("build-output", &[], 0);
    // Only git can show a project idle (Safe).
    std::fs::create_dir(root.join(".git")).unwrap();
    let idle = project(&root, "idle-crate", "Cargo.toml", "target", 10);
    let web = project(&root, "web", "package.json", "node_modules", 10);
    // An active crate inside the idle project's node_modules.
    let active = project(&web, "dep", "Cargo.toml", "target", 1);
    let session = TestSession::new();
    let discovery = discover_build_output(&session, &root, Arc::new(Git::untracked()));
    for category in &discovery.categories {
        assert_eq!(category.section, Some(FfiCleanSection::BuildOutput));
        assert_eq!(category.risk, FfiRisk::Review, "{}", category.id);
    }
    assert_eq!(risk_of(&discovery, &idle), FfiRisk::Safe);
    assert_eq!(risk_of(&discovery, &web), FfiRisk::Safe);
    assert_eq!(risk_of(&discovery, &active), FfiRisk::Review);

    let web_id = id_of(&discovery, "web/node_modules");
    let active_id = id_of(&discovery, "dep/target");
    // A Safe folder carrying an unselected Review one is excluded (PC-C3)...
    let preview = session.clean_preview(vec![web_id.clone()]).unwrap();
    assert!(preview.items.is_empty());
    assert_eq!(
        preview.excluded[0].reason,
        FfiExclusionReason::CoversUnselectedReview {
            candidate_ids: vec![active_id.clone()]
        }
    );
    // ...until the Review one is selected too.
    let both = session.clean_preview(vec![web_id, active_id]).unwrap();
    assert_eq!(both.items.len(), 1);
    assert!(both.items[0].path.ends_with("web/node_modules"));

    // A Safe item alone previews and moves; execution rebuilds the providers
    // from the stored options.
    let preview = session
        .clean_preview(vec![id_of(&discovery, "idle-crate/target")])
        .unwrap();
    let factory = |stored: &CleanOptions| {
        assert!(stored.all_build_output);
        build_output(stored, &root, Arc::new(Git::untracked()))
    };
    let trash = FakeTrash::default();
    let report = session
        .execute_clean(
            &preview.preview_id,
            &factory,
            &NotRunning,
            &trash,
            &CancellationToken::default(),
            &Progress::default(),
        )
        .unwrap();
    assert_eq!(outcomes(&report), vec![FfiItemOutcome::MovedToTrash]);
    assert_eq!(*trash.moved.lock().unwrap(), vec![idle]);
    remove(&root);
}
