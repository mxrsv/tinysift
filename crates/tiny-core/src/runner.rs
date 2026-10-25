//! Subprocess abstraction for code that shells out (`docker`, `go`, `lsof`,
//! `tmutil`, `mdfind`, `defaults`, `getconf`, ...). Real code uses
//! `RealRunner`; tests inject `MockRunner` so they don't depend on which
//! CLIs are installed in CI.
//!
//! `which`/`run` keep the original collapsing semantics that providers rely on.
//! New code uses `output`, which is bounded by a timeout and reports failures
//! as typed errors instead of empty results.
//!
//! `stream` is `output` for a long-running tool whose lines the caller wants
//! as they appear and whose run it may cancel ([`stream`]).
//!
//! A bare tool name is resolved once to an absolute path, which is then
//! spawned. `ToolLookup` decides where to look: the CLI searches `PATH`
//! only; the app also searches the Homebrew prefixes and, for the tools it
//! runs itself, their per-user install folders, because a Finder-launched
//! app inherits a minimal `PATH`.

use std::ffi::OsString;
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::AtomicBool;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

mod child_lifecycle;
mod isolation;
mod stream;

use child_lifecycle::ChildLifecycle;
use isolation::Isolation;

pub use stream::StreamEnd;

pub struct CommandOutput {
    pub success: bool,
    pub stdout: String,
}

/// A command that ran to completion. A non-zero exit is not an error here:
/// some tools (for example `lsof`) exit 1 to mean "nothing matched".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandOutcome {
    /// Exit code; `None` when the process was terminated by a signal.
    pub status: Option<i32>,
    pub stdout: String,
    /// Lossy UTF-8, truncated to `STDERR_LIMIT` bytes.
    pub stderr: String,
}

impl CommandOutcome {
    pub fn success(&self) -> bool {
        self.status == Some(0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CommandError {
    #[error("{0} was not found")]
    NotFound(String),
    #[error("failed to run {bin}: {detail}")]
    Spawn { bin: String, detail: String },
    #[error("{bin} did not finish within {timeout_ms} ms")]
    Timeout { bin: String, timeout_ms: u64 },
    #[error("{0} produced non-UTF-8 output")]
    NonUtf8(String),
}

/// Maximum stderr kept per command; the rest is drained and discarded.
pub const STDERR_LIMIT: u64 = 4096;
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Bound for tool calls made while discovering cleanup candidates.
pub const TOOL_TIMEOUT: Duration = Duration::from_secs(30);

/// Searched after `PATH` by the app (`ToolLookup::app`).
pub const APP_FALLBACK_DIRS: &[&str] = &["/opt/homebrew/bin", "/usr/local/bin"];

/// Set for every tool the app runs: a Corepack shim (pnpm installed through
/// nvm's Node) must never download a package manager or wait for an answer.
pub const APP_TOOL_ENV: &[(&str, &str)] = &[
    ("COREPACK_ENABLE_NETWORK", "0"),
    ("COREPACK_ENABLE_DOWNLOAD_PROMPT", "0"),
];

/// Where a bare tool name is searched.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolLookup {
    /// `None` reads `PATH` from the environment at resolution time.
    path: Option<OsString>,
    fallback_dirs: Vec<PathBuf>,
    /// The account's home, whose per-user install folders
    /// (`user_tool_dirs`) are searched after `PATH`.
    user_home: Option<PathBuf>,
    env: Vec<(&'static str, &'static str)>,
    /// Set for a tool that must not see the app's environment.
    isolation: Option<Isolation>,
}

impl ToolLookup {
    /// The CLI: `PATH` only, as before the lookup policy existed.
    pub fn path_only() -> Self {
        Self::default()
    }

    /// The app: `PATH`, then the Homebrew prefixes, with the per-user
    /// folders of the account's own home before them (nvm's pnpm) or after.
    pub fn app() -> Self {
        let fallbacks = APP_FALLBACK_DIRS.iter().map(PathBuf::from).collect();
        Self::new(None, fallbacks).with_user_tools(crate::clean::trash_plan::account_home())
    }

    /// Explicit `PATH` and fallbacks, so tests never touch the process env.
    pub fn new(path: Option<OsString>, fallback_dirs: Vec<PathBuf>) -> Self {
        Self {
            path,
            fallback_dirs,
            ..Self::default()
        }
    }

    /// Also searches the per-user install folders under `home` and runs
    /// every tool with `APP_TOOL_ENV`.
    pub fn with_user_tools(self, home: Option<PathBuf>) -> Self {
        Self {
            user_home: home,
            env: APP_TOOL_ENV.to_vec(),
            ..self
        }
    }

    /// Also runs every tool with `extra`, after the variables set so far.
    pub fn with_env(self, extra: &[(&'static str, &'static str)]) -> Self {
        let mut env = self.env.clone();
        env.extend_from_slice(extra);
        Self { env, ..self }
    }

    /// Runs every tool alone: the environment is cleared down to `HOME` (the
    /// account's, from `home`) and `path`, plus what `with_env` adds, and the
    /// tool starts in a session of its own with no controlling terminal. The
    /// search for the tool is unchanged; `path` is only what it runs with.
    /// Without a `home` the tool does not run, as if it were not found.
    pub fn isolated(self, home: Option<PathBuf>, path: &str) -> Self {
        Self {
            isolation: Some(Isolation::new(home, path)),
            ..self
        }
    }

    fn search_path(&self) -> Option<OsString> {
        self.path.clone().or_else(|| std::env::var_os("PATH"))
    }

    /// Absolute path of `bin`, or `bin` itself when it already contains a
    /// `/`. Resolution follows symlinks on purpose: Homebrew binaries are
    /// symlinks into the Cellar. This locates trusted tools, not discovered
    /// cleanup paths, so the clean module's `symlink_metadata` rule does
    /// not apply here.
    pub fn resolve(&self, bin: &str) -> Option<PathBuf> {
        if bin.contains('/') {
            return Some(PathBuf::from(bin));
        }
        let path = self.search_path().unwrap_or_default();
        let (before_fallbacks, after_fallbacks) = match &self.user_home {
            Some(home) => user_tool_dirs(home, bin),
            None => Default::default(),
        };
        std::env::split_paths(&path)
            .filter(|dir| !dir.as_os_str().is_empty())
            .chain(before_fallbacks)
            .chain(self.fallback_dirs.iter().cloned())
            .chain(after_fallbacks)
            .map(|dir| dir.join(bin))
            .find(|candidate| is_executable(candidate))
    }

    /// `PATH` for the child: tools found in a fallback dir (`npm`) or a
    /// per-user folder (nvm's `pnpm`) often look up siblings (`node`)
    /// through `PATH` themselves, so `tool_dir` is added too.
    fn child_path(&self, tool_dir: Option<&Path>) -> Option<OsString> {
        if self.fallback_dirs.is_empty() && self.path.is_none() && self.user_home.is_none() {
            return None;
        }
        let path = self.search_path().unwrap_or_default();
        let mut dirs: Vec<PathBuf> = std::env::split_paths(&path)
            .chain(self.fallback_dirs.iter().cloned())
            .collect();
        if let Some(dir) = tool_dir.filter(|dir| !dirs.iter().any(|d| d == dir)) {
            dirs.push(dir.to_path_buf());
        }
        std::env::join_paths(dirs).ok()
    }

    fn command(&self, bin: &str) -> Option<Command> {
        let resolved = self.resolve(bin)?;
        let mut command = Command::new(&resolved);
        if let Some(isolation) = &self.isolation {
            isolation.apply(&mut command)?;
        } else if let Some(path) = self.child_path(resolved.parent()) {
            command.env("PATH", path);
        }
        command.envs(self.env.iter().copied());
        Some(command)
    }
}

fn is_executable(path: &Path) -> bool {
    std::fs::metadata(path).is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
}

/// Per-user install folders of the tools the app runs itself (uv, pnpm):
/// the first list is searched right after `PATH`, the second after Homebrew.
/// Installers add them to a shell's `PATH`, which an app started from Finder
/// never sees. nvm's pnpm comes before Homebrew's, which can be a stale
/// install with its own store (pnpm 10's `store/v10` beside the `store/v11`
/// the user's nvm default uses).
fn user_tool_dirs(home: &Path, bin: &str) -> (Vec<PathBuf>, Vec<PathBuf>) {
    match bin {
        "uv" => (
            Vec::new(),
            vec![home.join(".local/bin"), home.join(".cargo/bin")],
        ),
        "pnpm" => (
            nvm_bin_dir(home, bin).into_iter().collect(),
            vec![
                home.join("Library/pnpm"),
                home.join(".local/share/pnpm"),
                home.join(".volta/bin"),
            ],
        ),
        _ => Default::default(),
    }
}

/// The `bin` folder of one nvm-installed Node that has `bin`: the version
/// `~/.nvm/alias/default` names (`24`, `v24.16.0`) when one matches, else
/// the highest version. An alias that names no version (`lts/*`) falls back
/// to the highest too.
fn nvm_bin_dir(home: &Path, bin: &str) -> Option<PathBuf> {
    let versions_dir = home.join(".nvm/versions/node");
    let mut versions: Vec<(Vec<u64>, PathBuf)> = std::fs::read_dir(&versions_dir)
        .ok()?
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().into_string().ok()?;
            let version = parse_version(&name)?;
            Some((version, versions_dir.join(name).join("bin")))
        })
        .filter(|(_, dir)| is_executable(&dir.join(bin)))
        .collect();
    versions.sort();
    let alias = std::fs::read_to_string(home.join(".nvm/alias/default")).ok();
    let default = alias
        .as_deref()
        .and_then(|alias| parse_version(alias.trim()))
        .and_then(|wanted| {
            versions
                .iter()
                .rev()
                .find(|(version, _)| version.starts_with(&wanted))
        });
    default.or(versions.last()).map(|(_, dir)| dir.clone())
}

/// `v24.16.0` or `24` as numbers; `None` for anything else.
fn parse_version(text: &str) -> Option<Vec<u64>> {
    let digits = text.strip_prefix('v').unwrap_or(text);
    digits.split('.').map(|part| part.parse().ok()).collect()
}

pub trait CommandRunner: Send + Sync {
    /// Returns true iff `which <bin>` succeeds.
    fn which(&self, bin: &str) -> bool;

    /// Runs `bin args...`, returning `(success, stdout)`. Failures (spawn
    /// error, non-utf8 output, non-zero exit) collapse to `success=false`,
    /// `stdout=""`.
    fn run(&self, bin: &str, args: &[&str]) -> CommandOutput;

    /// Runs `bin args...` with stdin closed, killing it after `timeout`.
    fn output(
        &self,
        bin: &str,
        args: &[&str],
        timeout: Duration,
    ) -> Result<CommandOutcome, CommandError>;

    /// Like `output`, but hands each line the command prints (stdout and
    /// stderr, as they arrive) to `on_line` and stops the command when
    /// `cancel` is raised. The default has no process to watch: it runs
    /// `output` and replays the lines afterwards, so fakes need not implement it.
    fn stream(
        &self,
        bin: &str,
        args: &[&str],
        timeout: Duration,
        cancel: Option<&AtomicBool>,
        on_line: &mut dyn FnMut(&str),
    ) -> Result<StreamEnd, CommandError> {
        if stream::is_cancelled(cancel) {
            return Ok(StreamEnd::Cancelled);
        }
        let outcome = self.output(bin, args, timeout)?;
        for line in outcome.stderr.lines().chain(outcome.stdout.lines()) {
            on_line(line);
        }
        Ok(StreamEnd::Finished(outcome))
    }
}

/// Runs tools found through `PATH` only (the CLI's behaviour).
pub struct RealRunner;

impl CommandRunner for RealRunner {
    fn which(&self, bin: &str) -> bool {
        ToolRunner::default().which(bin)
    }

    fn run(&self, bin: &str, args: &[&str]) -> CommandOutput {
        ToolRunner::default().run(bin, args)
    }

    fn output(
        &self,
        bin: &str,
        args: &[&str],
        timeout: Duration,
    ) -> Result<CommandOutcome, CommandError> {
        ToolRunner::default().output(bin, args, timeout)
    }

    fn stream(
        &self,
        bin: &str,
        args: &[&str],
        timeout: Duration,
        cancel: Option<&AtomicBool>,
        on_line: &mut dyn FnMut(&str),
    ) -> Result<StreamEnd, CommandError> {
        ToolRunner::default().stream(bin, args, timeout, cancel, on_line)
    }
}

/// Runs tools resolved through a `ToolLookup`.
#[derive(Debug, Clone, Default)]
pub struct ToolRunner {
    lookup: ToolLookup,
    /// Each `output` child gets its own process group, and a timeout kills
    /// the whole group: Corepack's pnpm, for one, runs the real work in a
    /// child of the process that was spawned. Off by default: with a
    /// terminal, Ctrl-C has to reach the child, which only its group shares.
    own_process_group: bool,
}

impl ToolRunner {
    /// For the app, which has no terminal: a timed-out tool leaves nothing
    /// running behind it.
    pub fn new(lookup: ToolLookup) -> Self {
        Self {
            lookup,
            own_process_group: true,
        }
    }

    /// Starts `command`: in a session of its own when the lookup isolates
    /// the tool (which makes it a group leader too), else in its own process
    /// group when this runner owns one.
    fn spawn(&self, command: Command, args: &[&str]) -> io::Result<ChildLifecycle> {
        if self.lookup.isolation.is_some() {
            ChildLifecycle::spawn_session(command, args)
        } else {
            ChildLifecycle::spawn(command, args, self.own_process_group)
        }
    }
}

impl CommandRunner for ToolRunner {
    fn which(&self, bin: &str) -> bool {
        self.lookup.resolve(bin).is_some()
    }

    fn run(&self, bin: &str, args: &[&str]) -> CommandOutput {
        let failed = CommandOutput {
            success: false,
            stdout: String::new(),
        };
        let Some(mut command) = self.lookup.command(bin) else {
            return failed;
        };
        let out = match command.args(args).output() {
            Ok(o) => o,
            Err(_) => return failed,
        };
        let stdout = String::from_utf8(out.stdout).unwrap_or_default();
        CommandOutput {
            success: out.status.success(),
            stdout,
        }
    }

    fn output(
        &self,
        bin: &str,
        args: &[&str],
        timeout: Duration,
    ) -> Result<CommandOutcome, CommandError> {
        let spawn_error = |error: io::Error| spawn_failure(bin, error);
        let command = self
            .lookup
            .command(bin)
            .ok_or_else(|| CommandError::NotFound(bin.to_string()))?;
        let mut child = self.spawn(command, args).map_err(spawn_error)?;
        // Drain both pipes before waiting so a chatty child cannot block on a
        // full pipe buffer. Keep the leader unreaped while descendants hold them.
        let stdout = drain(child.stdout(), u64::MAX);
        let stderr = drain(child.stderr(), STDERR_LIMIT);
        let deadline = Instant::now() + timeout;
        let status = loop {
            if stdout.is_finished()
                && stderr.is_finished()
                && child.exited().map_err(spawn_error)?
            {
                break child.wait().map_err(spawn_error)?;
            }
            if Instant::now() >= deadline {
                child.kill().map_err(spawn_error)?;
                // A descendant that deliberately left our group can still hold
                // pipes; detach drains rather than signal processes we do not own.
                return Err(CommandError::Timeout {
                    bin: bin.to_string(),
                    timeout_ms: timeout.as_millis() as u64,
                });
            }
            thread::sleep(POLL_INTERVAL);
        };
        let stdout =
            String::from_utf8(join(stdout)).map_err(|_| CommandError::NonUtf8(bin.to_string()))?;
        let stderr = String::from_utf8_lossy(&join(stderr)).into_owned();
        Ok(CommandOutcome {
            status: status.code(),
            stdout,
            stderr,
        })
    }

    fn stream(
        &self,
        bin: &str,
        args: &[&str],
        timeout: Duration,
        cancel: Option<&AtomicBool>,
        on_line: &mut dyn FnMut(&str),
    ) -> Result<StreamEnd, CommandError> {
        self.run_stream(bin, args, timeout, cancel, stream::INTERRUPT_GRACE, on_line)
    }
}

fn spawn_failure(bin: &str, error: io::Error) -> CommandError {
    match error.kind() {
        io::ErrorKind::NotFound => CommandError::NotFound(bin.to_string()),
        _ => CommandError::Spawn {
            bin: bin.to_string(),
            detail: error.to_string(),
        },
    }
}

/// Reads up to `limit` bytes on a background thread and discards the rest so
/// the child never blocks on a full pipe.
fn drain<R: Read + Send + 'static>(pipe: Option<R>, limit: u64) -> JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut kept = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = (&mut pipe).take(limit).read_to_end(&mut kept);
            let _ = io::copy(&mut pipe, &mut io::sink());
        }
        kept
    })
}

fn join(handle: JoinHandle<Vec<u8>>) -> Vec<u8> {
    handle.join().unwrap_or_default()
}

#[cfg(test)]
pub mod test_support {
    use super::*;
    use std::collections::HashMap;
    use std::sync::Mutex;

    /// Maps `(bin, args.join(" "))` to canned results. Unknown `run`
    /// invocations return `(false, "")`; unknown `output` invocations return
    /// `NotFound`. `run_calls` and `output_calls` record every invocation.
    pub struct MockRunner {
        pub which_set: Mutex<Vec<String>>,
        pub responses: Mutex<HashMap<String, (bool, String)>>,
        pub outputs: Mutex<HashMap<String, Result<CommandOutcome, CommandError>>>,
        pub run_calls: Mutex<Vec<String>>,
        pub output_calls: Mutex<Vec<String>>,
    }

    impl MockRunner {
        pub fn new() -> Self {
            Self {
                which_set: Mutex::new(Vec::new()),
                responses: Mutex::new(HashMap::new()),
                outputs: Mutex::new(HashMap::new()),
                run_calls: Mutex::new(Vec::new()),
                output_calls: Mutex::new(Vec::new()),
            }
        }
        pub fn with_which(self, bin: &str) -> Self {
            self.which_set.lock().unwrap().push(bin.to_string());
            self
        }
        pub fn with_response(self, bin: &str, args: &[&str], success: bool, stdout: &str) -> Self {
            let key = format!("{} {}", bin, args.join(" "));
            self.responses
                .lock()
                .unwrap()
                .insert(key, (success, stdout.to_string()));
            self
        }
        pub fn with_output(
            self,
            bin: &str,
            args: &[&str],
            result: Result<CommandOutcome, CommandError>,
        ) -> Self {
            let key = format!("{} {}", bin, args.join(" "));
            self.outputs.lock().unwrap().insert(key, result);
            self
        }
        /// `output` exits with `status` and prints `stdout`.
        pub fn with_exit(self, bin: &str, args: &[&str], status: i32, stdout: &str) -> Self {
            self.with_output(
                bin,
                args,
                Ok(CommandOutcome {
                    status: Some(status),
                    stdout: stdout.to_string(),
                    stderr: String::new(),
                }),
            )
        }
    }

    impl Default for MockRunner {
        fn default() -> Self {
            Self::new()
        }
    }

    impl CommandRunner for MockRunner {
        fn which(&self, bin: &str) -> bool {
            self.which_set.lock().unwrap().iter().any(|b| b == bin)
        }
        fn run(&self, bin: &str, args: &[&str]) -> CommandOutput {
            let key = format!("{} {}", bin, args.join(" "));
            self.run_calls.lock().unwrap().push(key.clone());
            match self.responses.lock().unwrap().get(&key) {
                Some((success, stdout)) => CommandOutput {
                    success: *success,
                    stdout: stdout.clone(),
                },
                None => CommandOutput {
                    success: false,
                    stdout: String::new(),
                },
            }
        }
        fn output(
            &self,
            bin: &str,
            args: &[&str],
            _timeout: Duration,
        ) -> Result<CommandOutcome, CommandError> {
            let key = format!("{} {}", bin, args.join(" "));
            self.output_calls.lock().unwrap().push(key.clone());
            self.outputs
                .lock()
                .unwrap()
                .get(&key)
                .cloned()
                .unwrap_or_else(|| Err(CommandError::NotFound(bin.to_string())))
        }
    }
}

#[cfg(test)]
mod tests;
