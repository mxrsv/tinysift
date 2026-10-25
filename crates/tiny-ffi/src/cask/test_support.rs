//! Fixtures for the cask tests: a fake `brew` that answers from a table and
//! spawns nothing, a fake app inventory and probe, and `brew info` JSON built
//! to the shape Homebrew 7 prints. Nothing here runs a real `brew`, reads
//! `/Applications` or asks Finder for anything.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tiny_core::cask::HomebrewStatus;
use tiny_core::runner::{CommandError, CommandOutcome, CommandOutput, CommandRunner, StreamEnd};
use tiny_core::uninstall::AppEntry;

use super::*;
use crate::FfiError;

/// Answers `brew <args>` from a table keyed by the arguments joined with
/// spaces, and records every call it gets.
pub(super) struct FakeBrew {
    pub(super) path: Mutex<Option<PathBuf>>,
    pub(super) home: Option<PathBuf>,
    replies: Mutex<HashMap<String, CommandOutcome>>,
    pub(super) calls: Mutex<Vec<String>>,
    /// Time between the lines of a streamed run, so a cancel raised by one
    /// line is seen before the next.
    pub(super) pace: Mutex<Duration>,
    /// Holds the call with these arguments open: says so on the sender, then
    /// waits on the receiver.
    pub(super) hold: Mutex<Option<Hold>>,
}

pub(super) struct Hold {
    pub(super) args: String,
    pub(super) entered: Sender<()>,
    pub(super) release: Receiver<()>,
}

impl FakeBrew {
    pub(super) fn reply(&self, args: &str, status: i32, stdout: &str, stderr: &str) {
        let outcome = CommandOutcome {
            status: Some(status),
            stdout: stdout.into(),
            stderr: stderr.into(),
        };
        self.replies.lock().unwrap().insert(args.into(), outcome);
    }

    pub(super) fn calls(&self) -> Vec<String> {
        self.calls.lock().unwrap().clone()
    }

    pub(super) fn clear_calls(&self) {
        self.calls.lock().unwrap().clear();
    }

    pub(super) fn spawned(&self, verb: &str) -> usize {
        let prefix = format!("{verb} ");
        self.calls()
            .iter()
            .filter(|c| c.starts_with(&prefix))
            .count()
    }
}

impl BrewHost for FakeBrew {
    fn status(&self) -> HomebrewStatus {
        match self.path.lock().unwrap().clone() {
            Some(path) => HomebrewStatus::Present(path),
            None => HomebrewStatus::Missing,
        }
    }
    fn runner(&self) -> &dyn CommandRunner {
        self
    }
    fn home(&self) -> Option<PathBuf> {
        self.home.clone()
    }
}

impl CommandRunner for FakeBrew {
    fn which(&self, _bin: &str) -> bool {
        false
    }
    fn run(&self, _bin: &str, _args: &[&str]) -> CommandOutput {
        panic!("the cask code never uses the collapsing runner");
    }
    fn stream(
        &self,
        _bin: &str,
        args: &[&str],
        _timeout: Duration,
        cancel: Option<&AtomicBool>,
        on_line: &mut dyn FnMut(&str),
    ) -> Result<StreamEnd, CommandError> {
        self.stream_reply(args, cancel, on_line)
    }
    fn output(
        &self,
        _bin: &str,
        args: &[&str],
        _timeout: Duration,
    ) -> Result<CommandOutcome, CommandError> {
        let call = args.join(" ");
        self.calls.lock().unwrap().push(call.clone());
        if let Some(hold) = self
            .hold
            .lock()
            .unwrap()
            .as_ref()
            .filter(|h| h.args == call)
        {
            hold.entered.send(()).unwrap();
            hold.release.recv().unwrap();
        }
        match self.replies.lock().unwrap().get(&call) {
            Some(outcome) => Ok(outcome.clone()),
            None => panic!("the fake brew was asked something unexpected: brew {call}"),
        }
    }
}

impl FakeBrew {
    /// `output`, then the lines of the reply handed out one by one, as the real
    /// runner does: a raised cancel flag ends the run between lines.
    fn stream_reply(
        &self,
        args: &[&str],
        cancel: Option<&AtomicBool>,
        on_line: &mut dyn FnMut(&str),
    ) -> Result<StreamEnd, CommandError> {
        let raised = || cancel.is_some_and(|flag| flag.load(Ordering::Acquire));
        let outcome = self.output("brew", args, Duration::from_secs(1))?;
        for line in outcome.stderr.lines().chain(outcome.stdout.lines()) {
            if raised() {
                return Ok(StreamEnd::Cancelled);
            }
            on_line(line);
            let pace = *self.pace.lock().unwrap();
            if !pace.is_zero() {
                std::thread::sleep(pace);
            }
        }
        Ok(if raised() {
            StreamEnd::Cancelled
        } else {
            StreamEnd::Finished(outcome)
        })
    }
}

/// An app inventory of fixtures; `unreadable` makes `identities` fail.
pub(super) struct FakeCatalog {
    pub(super) root: PathBuf,
    pub(super) apps: Mutex<Vec<AppEntry>>,
    pub(super) unreadable: AtomicBool,
    /// `None` makes the process name unknown.
    pub(super) executable: Mutex<Option<String>>,
    /// What `~/Applications` holds.
    pub(super) user_apps: Mutex<Vec<AppEntry>>,
}

impl AppCatalog for FakeCatalog {
    fn user_identities(&self, _home: &Path) -> Vec<AppEntry> {
        self.user_apps.lock().unwrap().clone()
    }
    fn applications_root(&self) -> &Path {
        &self.root
    }
    fn list(&self) -> Result<Vec<AppEntry>, FfiError> {
        Ok(self.apps.lock().unwrap().clone())
    }
    fn identities(&self) -> Result<Vec<AppEntry>, FfiError> {
        if self.unreadable.load(Ordering::SeqCst) {
            return Err(FfiError::Io {
                detail: "unreadable".into(),
            });
        }
        self.list()
    }
    fn executable(&self, _app: &Path) -> Option<String> {
        self.executable.lock().unwrap().clone()
    }
}

/// Process names that are open; `fails` makes the check unknown.
pub(super) struct FakeProbe {
    pub(super) running: Mutex<Vec<String>>,
    pub(super) fails: AtomicBool,
}

impl AppProbe for FakeProbe {
    fn probe(&self, name: &str) -> Result<bool, String> {
        if self.fails.load(Ordering::SeqCst) {
            return Err("probe unavailable".into());
        }
        Ok(self.running.lock().unwrap().iter().any(|n| n == name))
    }
}

#[derive(Default)]
pub(super) struct Listener {
    pub(super) steps: Mutex<Vec<FfiCaskProgress>>,
    /// Raised by the first step, to cancel a run that has just begun.
    pub(super) cancel_on_first_step: Mutex<Option<Arc<CancellationToken>>>,
    /// Raised by the first step that reaches this stage.
    pub(super) cancel_at_stage: Mutex<Option<(FfiCaskStage, Arc<CancellationToken>)>>,
}

impl CaskProgressListener for Listener {
    fn on_progress(&self, progress: FfiCaskProgress) {
        let progress_stage = progress.stage;
        self.steps.lock().unwrap().push(progress);
        if let Some(token) = self.cancel_on_first_step.lock().unwrap().take() {
            token.cancel();
        }
        let mut at_stage = self.cancel_at_stage.lock().unwrap();
        if at_stage
            .as_ref()
            .is_some_and(|(stage, _)| *stage == progress_stage)
        {
            if let Some((_, token)) = at_stage.take() {
                token.cancel();
            }
        }
    }
}

pub(super) const PROCESS: &str = "GhosttyExec";

pub(super) struct World {
    pub(super) root: PathBuf,
    pub(super) apps: PathBuf,
    pub(super) session: Arc<TinySession>,
    pub(super) brew: FakeBrew,
    pub(super) catalog: FakeCatalog,
    pub(super) probe: FakeProbe,
    pub(super) listener: Listener,
}

impl Drop for World {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

impl World {
    pub(super) fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let tmp = std::fs::canonicalize(std::env::temp_dir()).unwrap();
        let root = tmp.join(format!(
            "tiny-ffi-cask-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let apps = root.join("Applications");
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        let brew_path = root.join("prefix/bin/brew");
        Self {
            brew: FakeBrew {
                path: Mutex::new(Some(brew_path)),
                home: Some(root.join("home")),
                replies: Mutex::default(),
                calls: Mutex::default(),
                pace: Mutex::default(),
                hold: Mutex::default(),
            },
            catalog: FakeCatalog {
                root: apps.clone(),
                apps: Mutex::default(),
                unreadable: AtomicBool::new(false),
                executable: Mutex::new(Some(PROCESS.into())),
                user_apps: Mutex::default(),
            },
            probe: FakeProbe {
                running: Mutex::default(),
                fails: AtomicBool::new(false),
            },
            listener: Listener::default(),
            session: TinySession::new(),
            root,
            apps,
        }
    }

    pub(super) fn seams(&self) -> Seams<'_> {
        Seams {
            brew: &self.brew,
            catalog: &self.catalog,
            probe: &self.probe,
        }
    }

    pub(super) fn app(&self, name: &str) -> PathBuf {
        self.apps.join(format!("{name}.app"))
    }

    pub(super) fn install_app(&self, name: &str, bundle_id: &str) {
        std::fs::create_dir_all(self.app(name)).unwrap();
        self.catalog.apps.lock().unwrap().push(AppEntry {
            name: name.into(),
            path: self.app(name),
            bundle_id: Some(bundle_id.into()),
            size: 0,
            last_used_days: None,
        });
    }

    /// An app kept in `~/Applications` rather than `/Applications`.
    pub(super) fn keep_in_user_applications(&self, name: &str, bundle_id: &str) {
        self.catalog.user_apps.lock().unwrap().push(AppEntry {
            name: name.into(),
            path: self
                .root
                .join("home/Applications")
                .join(format!("{name}.app")),
            bundle_id: Some(bundle_id.into()),
            size: 0,
            last_used_days: None,
        });
    }

    pub(super) fn open(&self, process: &str) {
        self.probe.running.lock().unwrap().push(process.into());
    }

    pub(super) fn close_everything(&self) {
        self.probe.running.lock().unwrap().clear();
    }

    /// `brew info` for `token`: a cask whose app would land in `Applications`
    /// under the temp root.
    pub(super) fn info(&self, token: &str, spec: Spec<'_>) {
        let args = format!("info --cask --json=v2 {token}");
        let json = info_doc(&[cask_json(token, &self.apps, &spec)]);
        self.brew.reply(&args, 0, &json, "");
    }

    /// `brew info` for several tokens at once, in one call.
    pub(super) fn info_all(&self, specs: &[(&str, Spec<'_>)]) {
        let tokens: Vec<&str> = specs.iter().map(|(t, _)| *t).collect();
        let args = format!("info --cask --json=v2 {}", tokens.join(" "));
        let casks: Vec<String> = specs
            .iter()
            .map(|(token, spec)| cask_json(token, &self.apps, spec))
            .collect();
        self.brew.reply(&args, 0, &info_doc(&casks), "");
    }

    /// The default `brew outdated`, listing `(token, installed, current)`.
    pub(super) fn outdated_list(&self, listed: &[(&str, &str, &str)]) {
        let casks: Vec<String> = listed
            .iter()
            .map(|(token, installed, current)| {
                format!(
                    r#"{{"name":"{token}","installed_versions":["{installed}"],"current_version":"{current}","pinned":false,"pinned_version":null}}"#
                )
            })
            .collect();
        let json = format!(r#"{{"formulae":[],"casks":[{}]}}"#, casks.join(","));
        self.brew.reply("outdated --cask --json=v2", 0, &json, "");
    }

    pub(super) fn exec(
        &self,
        preview_id: &str,
        tokens: &[&str],
        token: &Arc<CancellationToken>,
    ) -> Result<FfiCaskReport, FfiCaskError> {
        let selected: Vec<String> = tokens.iter().map(|t| t.to_string()).collect();
        self.session
            .execute_casks(preview_id, &selected, &self.seams(), token, &self.listener)
    }

    pub(super) fn steps(&self) -> Vec<(String, FfiCaskStage, u64, u64)> {
        let steps = self.listener.steps.lock().unwrap();
        steps
            .iter()
            .map(|s| (s.token.clone(), s.stage, s.position, s.total))
            .collect()
    }
}

/// What a cask's `brew info` says besides its token.
#[derive(Clone)]
pub(super) struct Spec<'a> {
    pub(super) version: &'a str,
    pub(super) installed: Option<&'a str>,
    pub(super) bundle_version: Option<&'a str>,
    pub(super) outdated: bool,
    pub(super) app: &'a str,
    /// The JSON array of artifacts that go with the `app` one.
    pub(super) extra_artifacts: &'a str,
}

impl Default for Spec<'_> {
    fn default() -> Self {
        Self {
            version: "1.3.1",
            installed: None,
            bundle_version: None,
            outdated: false,
            app: "Ghostty",
            extra_artifacts: "",
        }
    }
}

fn cask_json(token: &str, apps: &Path, spec: &Spec<'_>) -> String {
    let quoted = |value: Option<&str>| value.map_or("null".to_string(), |v| format!("\"{v}\""));
    let extra = if spec.extra_artifacts.is_empty() {
        String::new()
    } else {
        format!(",{}", spec.extra_artifacts)
    };
    format!(
        r#"{{"token":"{token}","name":["{token}"],"url":"https://downloads.example.com/{token}.dmg","version":"{version}","installed":{installed},"bundle_short_version":{bundle},"outdated":{outdated},"artifacts":[{{"app":["{app}.app"],"target":"{target}"}}{extra}]}}"#,
        version = spec.version,
        installed = quoted(spec.installed),
        bundle = quoted(spec.bundle_version),
        outdated = spec.outdated,
        app = spec.app,
        target = apps.join(format!("{}.app", spec.app)).display(),
    )
}

fn info_doc(casks: &[String]) -> String {
    format!(r#"{{"formulae":[],"casks":[{}]}}"#, casks.join(","))
}

pub(super) fn failure_kind(error: FfiCaskError) -> FfiCaskFailureKind {
    match error {
        FfiCaskError::Failed { failure } => failure.kind,
        other => panic!("expected a typed failure, got {other:?}"),
    }
}
