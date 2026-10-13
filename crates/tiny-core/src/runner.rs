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
//! only; the app also searches the Homebrew prefixes, because a
//! Finder-launched app inherits a minimal `PATH`.

use std::ffi::OsString;
use std::io::{self, Read};
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
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

/// Where a bare tool name is searched.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ToolLookup {
    /// `None` reads `PATH` from the environment at resolution time.
    path: Option<OsString>,
    fallback_dirs: Vec<PathBuf>,
}

impl ToolLookup {
    /// The CLI: `PATH` only, as before the lookup policy existed.
    pub fn path_only() -> Self {
        Self::default()
    }

    /// The app: `PATH`, then the Homebrew prefixes.
    pub fn app() -> Self {
        Self::new(None, APP_FALLBACK_DIRS.iter().map(PathBuf::from).collect())
    }

    /// Explicit `PATH` and fallbacks, so tests never touch the process env.
    pub fn new(path: Option<OsString>, fallback_dirs: Vec<PathBuf>) -> Self {
        Self {
            path,
            fallback_dirs,
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
        std::env::split_paths(&path)
            .filter(|dir| !dir.as_os_str().is_empty())
            .chain(self.fallback_dirs.iter().cloned())
            .map(|dir| dir.join(bin))
            .find(|candidate| {
                std::fs::metadata(candidate)
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            })
    }

    /// `PATH` for the child: tools found in a fallback dir (`npm`) often
    /// look up siblings (`node`) through `PATH` themselves.
    fn child_path(&self) -> Option<OsString> {
        if self.fallback_dirs.is_empty() && self.path.is_none() {
            return None;
        }
        let path = self.search_path().unwrap_or_default();
        let dirs = std::env::split_paths(&path).chain(self.fallback_dirs.iter().cloned());
        std::env::join_paths(dirs).ok()
    }

    fn command(&self, bin: &str) -> Option<Command> {
        let resolved = self.resolve(bin)?;
        let mut command = Command::new(resolved);
        if let Some(path) = self.child_path() {
            command.env("PATH", path);
        }
        Some(command)
    }
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
}

impl ToolRunner {
    pub fn new(lookup: ToolLookup) -> Self {
        Self { lookup }
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
