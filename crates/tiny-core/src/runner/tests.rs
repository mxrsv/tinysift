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

/// A disposable descendant holds the runner's pipes until cleanup or our stop file.
/// The stop file also cleans up the fixture against the broken implementation.
pub(super) struct InheritedPipeFixture {
    directory: tempfile::TempDir,
}

impl InheritedPipeFixture {
    pub(super) fn new() -> Self {
        Self {
            directory: tempfile::tempdir().unwrap(),
        }
    }

    pub(super) fn args(&self, leader_waits: bool, noisy: bool) -> Vec<String> {
        let script = r#"
/bin/sh -c '
    trap "" INT
    echo $$ > "$1"
    : > "$2"
    while [ -d "${3%/*}" ] && [ ! -e "$3" ]; do
        if [ "$4" = noisy ]; then echo inherited-output; else /bin/sleep 0.01; fi
    done
' descendant "$1" "$2" "$3" "$4" &
while [ ! -e "$2" ]; do /bin/sleep 0.01; done
if [ "$5" = wait ]; then
    trap 'exit 0' INT
    echo started
    wait
fi
"#;
        vec![
            "-c".into(),
            script.into(),
            "leader".into(),
            self.path("pid"),
            self.path("ready"),
            self.path("stop"),
            if noisy { "noisy" } else { "quiet" }.into(),
            if leader_waits { "wait" } else { "exit" }.into(),
        ]
    }

    fn path(&self, name: &str) -> String {
        self.directory
            .path()
            .join(name)
            .to_str()
            .unwrap()
            .to_string()
    }

    pub(super) fn survived(&self) -> bool {
        let pid: libc::pid_t = std::fs::read_to_string(self.path("pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        // SAFETY: signal 0 does not change any process. The stop file, not a PID signal,
        // releases this disposable fixture if the implementation left it running.
        let survived = unsafe { libc::kill(pid, 0) } == 0 && !gone(pid);
        std::fs::write(self.path("stop"), "").unwrap();
        survived
    }
}

impl Drop for InheritedPipeFixture {
    fn drop(&mut self) {
        let _ = std::fs::write(self.path("stop"), "");
    }
}

#[test]
fn app_timeout_stops_inherited_pipe_child_after_leader_exits() {
    let fixture = InheritedPipeFixture::new();
    let args = fixture.args(false, false);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let started = Instant::now();
    let result =
        ToolRunner::new(ToolLookup::default()).output(SH, &args, Duration::from_millis(150));
    let survived = fixture.survived();
    assert!(
        matches!(result, Err(CommandError::Timeout { .. })),
        "{result:?}"
    );
    assert!(
        !survived,
        "descendant outlived app timeout after leader exit"
    );
    assert!(started.elapsed() < Duration::from_secs(3));
}
