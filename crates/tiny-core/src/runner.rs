//! Subprocess abstraction for code that shells out (`docker`, `go`, `lsof`,
//! `tmutil`, `mdfind`, `defaults`, `getconf`, ...). Real code uses
//! `RealRunner`; tests inject `MockRunner` so they don't depend on which
//! CLIs are installed in CI.
//!
//! `which`/`run` keep the original collapsing semantics that providers rely on.
//! New code uses `output`, which is bounded by a timeout and reports failures
//! as typed errors instead of empty results.
//!
//! A bare tool name is resolved once to an absolute path, which is then
//! spawned. `ToolLookup` decides where to look: the CLI searches `PATH`
//! only; the app also searches the Homebrew prefixes and, for the tools it
//! runs itself, their per-user install folders, because a Finder-launched
//! app inherits a minimal `PATH`.

use std::ffi::OsString;
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

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
        if let Some(path) = self.child_path(resolved.parent()) {
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
        let spawn_error = |error: io::Error| match error.kind() {
            io::ErrorKind::NotFound => CommandError::NotFound(bin.to_string()),
            _ => CommandError::Spawn {
                bin: bin.to_string(),
                detail: error.to_string(),
            },
        };
        let mut command = self
            .lookup
            .command(bin)
            .ok_or_else(|| CommandError::NotFound(bin.to_string()))?;
        if self.own_process_group {
            command.process_group(0);
        }
        let mut child = command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(spawn_error)?;
        // Drain both pipes before waiting so a chatty child cannot block on a
        // full pipe buffer.
        let stdout = drain(child.stdout.take(), u64::MAX);
        let stderr = drain(child.stderr.take(), STDERR_LIMIT);
        let deadline = Instant::now() + timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() >= deadline => {
                    if self.own_process_group {
                        // The group id is the child's pid, and the child has
                        // not been reaped, so it cannot belong to anyone else.
                        // SAFETY: kill(2) with a negated pid, no memory involved.
                        unsafe { libc::kill(-(child.id() as libc::pid_t), libc::SIGKILL) };
                    }
                    let _ = child.kill();
                    let _ = child.wait();
                    // ponytail: drain threads are detached here; a grandchild
                    // that inherited the pipes keeps them alive until it exits.
                    return Err(CommandError::Timeout {
                        bin: bin.to_string(),
                        timeout_ms: timeout.as_millis() as u64,
                    });
                }
                Ok(None) => thread::sleep(POLL_INTERVAL),
                Err(error) => return Err(spawn_error(error)),
            }
        };
        // An exited child may leave its stdout/stderr open in a grandchild.
        // Joining is safe only once both drains finish, within the same deadline.
        while !stdout.is_finished() || !stderr.is_finished() {
            if Instant::now() >= deadline {
                return Err(CommandError::Timeout {
                    bin: bin.to_string(),
                    timeout_ms: timeout.as_millis() as u64,
                });
            }
            thread::sleep(POLL_INTERVAL);
        }
        let stdout =
            String::from_utf8(join(stdout)).map_err(|_| CommandError::NonUtf8(bin.to_string()))?;
        let stderr = String::from_utf8_lossy(&join(stderr)).into_owned();
        Ok(CommandOutcome {
            status: status.code(),
            stdout,
            stderr,
        })
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
mod tests {
    use super::*;

    const SH: &str = "/bin/sh";
    const GENEROUS: Duration = Duration::from_secs(5);

    #[test]
    fn output_reports_exit_code_stdout_and_stderr() {
        let outcome = RealRunner
            .output(SH, &["-c", "echo out; echo err >&2; exit 3"], GENEROUS)
            .expect("sh runs");
        assert_eq!(outcome.status, Some(3));
        assert!(!outcome.success());
        assert_eq!(outcome.stdout, "out\n");
        assert_eq!(outcome.stderr, "err\n");
    }

    #[test]
    fn output_times_out_and_kills_the_child() {
        let started = Instant::now();
        let result = RealRunner.output(SH, &["-c", "sleep 5"], Duration::from_millis(100));
        assert!(matches!(
            result,
            Err(CommandError::Timeout {
                timeout_ms: 100,
                ..
            })
        ));
        assert!(started.elapsed() < Duration::from_secs(3));
    }

    /// Whether `pid` is gone, giving the kernel a moment to reap it.
    fn gone(pid: libc::pid_t) -> bool {
        (0..40).any(|_| {
            // SAFETY: signal 0 only asks whether the process exists.
            let alive = unsafe { libc::kill(pid, 0) } == 0;
            if alive {
                thread::sleep(Duration::from_millis(50));
            }
            !alive
        })
    }

    #[test]
    fn a_timeout_in_the_app_kills_what_the_tool_started_and_in_a_terminal_only_the_tool() {
        let dir = tempfile::tempdir().unwrap();
        let script = |pidfile: &Path| format!("sleep 20 & echo $! > '{}'; wait", pidfile.display());
        let started = |pidfile: &Path| -> libc::pid_t {
            std::fs::read_to_string(pidfile)
                .unwrap()
                .trim()
                .parse()
                .unwrap()
        };

        // The app's runner: its own process group, killed whole.
        let app_pid = dir.path().join("app.pid");
        let result = ToolRunner::new(ToolLookup::default()).output(
            SH,
            &["-c", &script(&app_pid)],
            Duration::from_secs(1),
        );
        assert!(matches!(result, Err(CommandError::Timeout { .. })));
        let grandchild = started(&app_pid);
        let killed = gone(grandchild);
        if !killed {
            unsafe { libc::kill(grandchild, libc::SIGKILL) };
        }
        assert!(killed, "the tool's child outlived the timeout");

        // The CLI's runner shares the terminal's group so Ctrl-C reaches the
        // tool; a timeout kills the tool only.
        let cli_pid = dir.path().join("cli.pid");
        let result = RealRunner.output(SH, &["-c", &script(&cli_pid)], Duration::from_secs(1));
        assert!(matches!(result, Err(CommandError::Timeout { .. })));
        let grandchild = started(&cli_pid);
        // SAFETY: signal 0 only asks whether the process exists.
        let survives = unsafe { libc::kill(grandchild, 0) } == 0;
        unsafe { libc::kill(grandchild, libc::SIGKILL) };
        assert!(survives, "the CLI runner must not change process groups");
    }

    #[test]
    fn output_deadline_includes_pipes_inherited_by_a_grandchild() {
        let started = Instant::now();
        let result = RealRunner.output(SH, &["-c", "sleep 1 & exit 0"], Duration::from_millis(100));
        assert!(matches!(
            result,
            Err(CommandError::Timeout {
                timeout_ms: 100,
                ..
            })
        ));
        assert!(started.elapsed() < Duration::from_millis(800));
    }

    #[test]
    fn output_reports_missing_binary() {
        let result = RealRunner.output("/nonexistent/tiny-test-binary", &[], GENEROUS);
        assert!(matches!(result, Err(CommandError::NotFound(_))));
    }

    #[test]
    fn output_rejects_non_utf8_stdout() {
        let result = RealRunner.output(SH, &["-c", "printf '\\377'"], GENEROUS);
        assert!(matches!(result, Err(CommandError::NonUtf8(_))));
    }

    fn fixture_dir(label: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "tiny-runner-{label}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn write_tool(dir: &std::path::Path, name: &str, body: &str) {
        let path = dir.join(name);
        std::fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn path_only_lookup_misses_a_tool_outside_path() {
        let prefix = fixture_dir("prefix");
        write_tool(&prefix, "tiny-fake-tool", "echo hi");
        let empty = fixture_dir("empty-path");
        let cli = ToolLookup::new(Some(empty.clone().into_os_string()), Vec::new());
        assert_eq!(cli.resolve("tiny-fake-tool"), None);
        let runner = ToolRunner::new(cli);
        assert!(!runner.which("tiny-fake-tool"));
        assert!(matches!(
            runner.output("tiny-fake-tool", &[], GENEROUS),
            Err(CommandError::NotFound(_))
        ));
        let _ = std::fs::remove_dir_all(&prefix);
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn app_lookup_finds_a_tool_in_a_fallback_prefix_and_extends_child_path() {
        let prefix = fixture_dir("prefix");
        write_tool(&prefix, "tiny-fake-tool", "echo \"$PATH\"");
        let path = OsString::from("/usr/bin:/bin");
        let app = ToolLookup::new(Some(path), vec![prefix.clone()]);
        assert_eq!(
            app.resolve("tiny-fake-tool"),
            Some(prefix.join("tiny-fake-tool"))
        );
        let outcome = ToolRunner::new(app)
            .output("tiny-fake-tool", &[], GENEROUS)
            .expect("fixture tool runs");
        assert!(outcome.success());
        assert!(
            outcome.stdout.trim().ends_with(prefix.to_str().unwrap()),
            "child PATH includes the fallback dir: {}",
            outcome.stdout
        );
        let _ = std::fs::remove_dir_all(&prefix);
    }

    /// A fake tool at `dir/name` that prints its `PATH` and Corepack flags.
    fn write_probe(dir: &std::path::Path, name: &str) {
        std::fs::create_dir_all(dir).unwrap();
        let body = "echo \"$PATH|$COREPACK_ENABLE_NETWORK|$COREPACK_ENABLE_DOWNLOAD_PROMPT\"";
        write_tool(dir, name, body);
    }

    fn nvm_bin(home: &std::path::Path, version: &str) -> PathBuf {
        home.join(".nvm/versions/node").join(version).join("bin")
    }

    #[test]
    fn the_app_finds_uv_and_pnpm_in_per_user_folders_only_after_path() {
        let home = fixture_dir("user-tools");
        let empty = fixture_dir("user-tools-path");
        let lookup = |path: &std::path::Path| {
            ToolLookup::new(Some(path.as_os_str().to_owned()), Vec::new())
                .with_user_tools(Some(home.clone()))
        };
        write_probe(&home.join(".local/bin"), "uv");
        write_probe(&home.join(".local/bin"), "npm");
        write_probe(&home.join("Library/pnpm"), "pnpm");
        assert_eq!(
            lookup(&empty).resolve("uv"),
            Some(home.join(".local/bin/uv"))
        );
        assert_eq!(
            lookup(&empty).resolve("pnpm"),
            Some(home.join("Library/pnpm/pnpm"))
        );
        assert_eq!(lookup(&empty).resolve("npm"), None, "only uv and pnpm");
        write_probe(&empty, "uv");
        assert_eq!(
            lookup(&empty).resolve("uv"),
            Some(empty.join("uv")),
            "PATH first"
        );
        let cli = ToolLookup::new(Some(OsString::new()), Vec::new());
        assert_eq!(cli.resolve("pnpm"), None, "the CLI never looks there");
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn nvm_pnpm_comes_from_the_default_alias_or_the_highest_version() {
        let home = fixture_dir("nvm");
        let empty = fixture_dir("nvm-path");
        let lookup = ToolLookup::new(Some(empty.as_os_str().to_owned()), Vec::new())
            .with_user_tools(Some(home.clone()));
        for version in ["v9.1.0", "v10.0.0"] {
            write_probe(&nvm_bin(&home, version), "pnpm");
        }
        std::fs::create_dir_all(nvm_bin(&home, "v24.16.0")).unwrap();
        let pnpm = |version: &str| Some(nvm_bin(&home, version).join("pnpm"));
        assert_eq!(lookup.resolve("pnpm"), pnpm("v10.0.0"), "numeric, not text");
        std::fs::create_dir_all(home.join(".nvm/alias")).unwrap();
        std::fs::write(home.join(".nvm/alias/default"), "9\n").unwrap();
        assert_eq!(lookup.resolve("pnpm"), pnpm("v9.1.0"), "the default alias");
        std::fs::write(home.join(".nvm/alias/default"), "24").unwrap();
        assert_eq!(
            lookup.resolve("pnpm"),
            pnpm("v10.0.0"),
            "the default has no pnpm"
        );
        let _ = std::fs::remove_dir_all(&home);
        let _ = std::fs::remove_dir_all(&empty);
    }

    #[test]
    fn nvm_pnpm_wins_over_homebrew_but_not_over_path() {
        let home = fixture_dir("nvm-first");
        let homebrew = fixture_dir("nvm-first-homebrew");
        let path = fixture_dir("nvm-first-path");
        let lookup = || {
            ToolLookup::new(Some(path.as_os_str().to_owned()), vec![homebrew.clone()])
                .with_user_tools(Some(home.clone()))
        };
        for dir in [&homebrew, &home.join("Library/pnpm")] {
            write_probe(dir, "pnpm");
        }
        write_probe(&nvm_bin(&home, "v24.16.0"), "pnpm");
        assert_eq!(
            lookup().resolve("pnpm"),
            Some(nvm_bin(&home, "v24.16.0").join("pnpm")),
            "the user's nvm pnpm, not a stale Homebrew one"
        );
        write_probe(&homebrew, "uv");
        write_probe(&home.join(".local/bin"), "uv");
        assert_eq!(
            lookup().resolve("uv"),
            Some(homebrew.join("uv")),
            "uv keeps Homebrew first"
        );
        write_probe(&path, "pnpm");
        assert_eq!(
            lookup().resolve("pnpm"),
            Some(path.join("pnpm")),
            "PATH first"
        );
        for dir in [&home, &homebrew, &path] {
            let _ = std::fs::remove_dir_all(dir);
        }
    }

    #[test]
    fn a_per_user_tool_finds_its_node_and_never_downloads_through_corepack() {
        let home = fixture_dir("corepack");
        let bin = nvm_bin(&home, "v24.16.0");
        write_probe(&bin, "pnpm");
        let lookup = ToolLookup::new(Some(OsString::from("/usr/bin:/bin")), Vec::new())
            .with_user_tools(Some(home.clone()));
        let outcome = ToolRunner::new(lookup)
            .output("pnpm", &[], GENEROUS)
            .expect("fixture tool runs");
        let printed = outcome.stdout.trim().to_string();
        let (path, flags) = printed.split_once('|').unwrap();
        assert!(
            std::env::split_paths(path).any(|dir| dir == bin),
            "its own folder is on PATH for `node`: {path}"
        );
        assert_eq!(flags, "0|0");
        let _ = std::fs::remove_dir_all(&home);
    }

    #[test]
    fn lookup_skips_non_executable_files() {
        let prefix = fixture_dir("noexec");
        std::fs::write(prefix.join("tiny-fake-tool"), "#!/bin/sh\n").unwrap();
        let lookup = ToolLookup::new(Some(OsString::new()), vec![prefix.clone()]);
        assert_eq!(lookup.resolve("tiny-fake-tool"), None);
        let _ = std::fs::remove_dir_all(&prefix);
    }

    #[test]
    fn output_caps_stderr_without_blocking() {
        let outcome = RealRunner
            .output(
                SH,
                &["-c", "head -c 200000 /dev/zero | tr '\\0' x >&2"],
                GENEROUS,
            )
            .expect("sh runs");
        assert!(outcome.success());
        assert_eq!(outcome.stderr.len() as u64, STDERR_LIMIT);
    }
}
