//! Bare `sift` opens the TUI only in a terminal of 60×18 or more; everywhere
//! else it prints help and exits 2 as before. The pseudo-terminal tests run
//! the binary under macOS `script` and check the terminal is given back
//! (alternate screen left, cursor shown, line mode restored) after `q` and
//! after SIGTERM.
//!
//! What the launched binary reads: it opens on Clean, so it runs the same
//! read-only discovery as `sift clean --dry-run` (the categories' locations
//! under HOME and system-wide, and the read-only queries of the tools some
//! categories use) and reads the startup volume's free space. No test
//! presses a key that cleans, so nothing is changed. HOME is a temporary
//! directory: the categories under it find nothing, and the `--icons`
//! setting is written there, never to the user's own settings.

use std::io::{Read, Write};
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tempfile::TempDir;

const SCRIPT: &str = "/usr/bin/script";
const ENTER_ALTERNATE: &str = "\x1b[?1049h";
const LEAVE_ALTERNATE: &str = "\x1b[?1049l";
const SHOW_CURSOR: &str = "\x1b[?25h";
const LIMIT: Duration = Duration::from_secs(20);

fn binary_named(name: &str) -> (TempDir, PathBuf) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(name);
    symlink(assert_cmd::cargo::cargo_bin("tinysift"), &path).unwrap();
    (dir, path)
}

#[test]
fn without_a_terminal_bare_sift_prints_help_and_exits_2() {
    for name in ["sift", "tinysift"] {
        let (_dir, bin) = binary_named(name);
        let output = Command::new(&bin).stdin(Stdio::null()).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{name}");
        assert!(output.stdout.is_empty(), "{name}");
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(stderr.starts_with("A small, practical CLI"), "{stderr}");
        assert!(
            stderr.contains(&format!("Usage: {name} <COMMAND>")),
            "{stderr}"
        );
        assert!(
            !stderr.contains('\x1b'),
            "no terminal control without a terminal"
        );
    }
}

#[test]
fn without_a_terminal_icons_is_not_a_flag_of_the_commands() {
    let (_dir, bin) = binary_named("sift");
    let output = Command::new(&bin)
        .args(["--icons", "nerd"])
        .stdin(Stdio::null())
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected argument '--icons'"));
}

/// The binary under a pseudo-terminal of `cols`×`rows`; the shell prints its
/// PID, runs sift, then prints sift's status and the line settings.
struct Pty {
    child: Child,
    output: Arc<Mutex<Vec<u8>>>,
    _home: TempDir,
}

impl Pty {
    fn start(bin: &Path, cols: u16, rows: u16) -> Option<Self> {
        Self::start_in(tempfile::tempdir().unwrap(), bin, "", cols, rows)
    }

    fn start_in(home: TempDir, bin: &Path, args: &str, cols: u16, rows: u16) -> Option<Self> {
        if !Path::new(SCRIPT).exists() {
            eprintln!("skipped: {SCRIPT} is missing");
            return None;
        }
        let shell = format!(
            "stty cols {cols} rows {rows}; echo PID $$; '{}' {args}; echo STATUS $?; stty -a",
            bin.display()
        );
        let mut child = Command::new(SCRIPT)
            .args(["-q", "/dev/null", "sh", "-c", &shell])
            .env("HOME", home.path())
            .env_remove("NO_COLOR")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let output = Arc::new(Mutex::new(Vec::new()));
        let mut stdout = child.stdout.take().unwrap();
        let sink = output.clone();
        std::thread::spawn(move || {
            let mut chunk = [0u8; 4096];
            while let Ok(n) = stdout.read(&mut chunk) {
                if n == 0 {
                    break;
                }
                sink.lock().unwrap().extend_from_slice(&chunk[..n]);
            }
        });
        Some(Self {
            child,
            output,
            _home: home,
        })
    }

    fn text(&self) -> String {
        String::from_utf8_lossy(&self.output.lock().unwrap()).into_owned()
    }

    fn wait_for(&self, needle: &str) -> String {
        let started = Instant::now();
        loop {
            let text = self.text();
            if text.contains(needle) {
                return text;
            }
            assert!(
                started.elapsed() < LIMIT,
                "never printed {needle:?}:\n{text:?}"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn press(&mut self, keys: &str) {
        let stdin = self.child.stdin.as_mut().unwrap();
        stdin.write_all(keys.as_bytes()).unwrap();
        stdin.flush().unwrap();
    }

    /// The PID of sift: the only child of the shell that printed `PID`.
    fn sift_pid(&self) -> String {
        let text = self.wait_for("PID ");
        let shell = text
            .split("PID ")
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
            .unwrap()
            .to_string();
        let output = Command::new("pgrep").args(["-P", &shell]).output().unwrap();
        String::from_utf8_lossy(&output.stdout).trim().to_string()
    }

    fn finish(self) -> String {
        self.finish_keeping_home().0
    }

    fn finish_keeping_home(mut self) -> (String, TempDir) {
        let text = self.wait_for("speed");
        let started = Instant::now();
        while self.child.try_wait().unwrap().is_none() {
            assert!(started.elapsed() < LIMIT, "script did not end");
            std::thread::sleep(Duration::from_millis(20));
        }
        let home = std::mem::replace(&mut self._home, tempfile::tempdir().unwrap());
        (text, home)
    }
}

impl Drop for Pty {
    fn drop(&mut self) {
        let _ = self.child.kill();
    }
}

/// After the TUI: alternate screen left, cursor shown, line mode and echo back.
fn assert_restored(text: &str) {
    let after = text.split(LEAVE_ALTERNATE).nth(1).unwrap_or_default();
    assert!(text.contains(LEAVE_ALTERNATE), "{text:?}");
    let tail = &text[text.rfind(ENTER_ALTERNATE).unwrap()..];
    assert!(tail.contains(SHOW_CURSOR), "{tail:?}");
    let settings = after.split("STATUS").nth(1).unwrap_or_default();
    let flags: Vec<&str> = settings.split_whitespace().collect();
    for flag in ["icanon", "echo", "isig"] {
        assert!(flags.contains(&flag), "{flag} not restored:\n{settings}");
    }
}

#[test]
fn q_leaves_the_tui_and_gives_the_terminal_back() {
    let (_dir, bin) = binary_named("sift");
    let Some(mut pty) = Pty::start(&bin, 120, 34) else {
        return;
    };
    let screen = pty.wait_for("tab page");
    assert!(screen.contains(ENTER_ALTERNATE));
    assert!(screen.contains("Processes"));
    pty.press("q");
    let text = pty.finish();
    assert!(text.contains("STATUS 0"), "{text:?}");
    assert_restored(&text);
}

#[test]
fn ctrl_c_is_a_key_in_raw_mode_and_quits_like_q() {
    let (_dir, bin) = binary_named("tinysift");
    let Some(mut pty) = Pty::start(&bin, 80, 24) else {
        return;
    };
    pty.wait_for("tab page");
    pty.press("\u{3}");
    let text = pty.finish();
    assert!(text.contains("STATUS 0"), "{text:?}");
    assert_restored(&text);
}

#[test]
fn the_smallest_terminal_opens_the_tui_and_q_still_quits() {
    let (_dir, bin) = binary_named("sift");
    let Some(mut pty) = Pty::start(&bin, 60, 18) else {
        return;
    };
    pty.wait_for("q quit");
    pty.press("q");
    let text = pty.finish();
    assert!(text.contains(ENTER_ALTERNATE), "{text:?}");
    assert!(text.contains("STATUS 0"), "{text:?}");
    assert_restored(&text);
}

#[test]
fn sigterm_leaves_the_tui_and_gives_the_terminal_back() {
    let (_dir, bin) = binary_named("sift");
    let Some(pty) = Pty::start(&bin, 80, 24) else {
        return;
    };
    pty.wait_for("tab page");
    let pid = pty.sift_pid();
    assert!(!pid.is_empty());
    let killed = Command::new("kill").args(["-TERM", &pid]).status().unwrap();
    assert!(killed.success());
    let text = pty.finish();
    assert!(text.contains("STATUS 0"), "{text:?}");
    assert_restored(&text);
}

#[test]
fn a_small_terminal_gets_one_line_and_no_tui() {
    let (_dir, bin) = binary_named("sift");
    let Some(pty) = Pty::start(&bin, 59, 18) else {
        return;
    };
    let text = pty.finish();
    assert!(!text.contains(ENTER_ALTERNATE), "{text:?}");
    assert!(
        text.contains("sift needs a terminal of at least 60×18; this one is 59×18."),
        "{text:?}"
    );
    assert!(text.contains("sift <command>"));
    assert!(text.contains("STATUS 2"), "{text:?}");
}

#[test]
fn icons_nerd_is_drawn_and_remembered_for_the_next_bare_run() {
    const BROOM: &str = "\u{F00E2}";
    let (_dir, bin) = binary_named("sift");
    let home = tempfile::tempdir().unwrap();
    let Some(mut pty) = Pty::start_in(home, &bin, "--icons nerd", 120, 34) else {
        return;
    };
    assert!(pty.wait_for("tab page").contains(BROOM));
    pty.press("q");
    let (text, home) = pty.finish_keeping_home();
    assert!(text.contains("STATUS 0"), "{text:?}");
    let stored = std::fs::read_to_string(home.path().join(".tiny-cli/tui-settings.json")).unwrap();
    assert!(stored.contains("\"nerd\""), "{stored}");

    let Some(mut again) = Pty::start_in(home, &bin, "", 120, 34) else {
        return;
    };
    assert!(again.wait_for("tab page").contains(BROOM), "remembered");
    again.press("q");
    let (_, home) = again.finish_keeping_home();
    let Some(mut plain) = Pty::start_in(home, &bin, "--icons unicode", 120, 34) else {
        return;
    };
    assert!(!plain.wait_for("tab page").contains(BROOM));
    plain.press("q");
    plain.finish();
}
