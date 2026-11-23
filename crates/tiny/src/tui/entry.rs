//! When bare `sift` opens the TUI, and what it prints when it does not.
//!
//! Only a bare invocation (optionally with `--icons`) whose stdin and stdout
//! are both terminals is a TUI launch. Every other invocation, including bare
//! `sift` in a pipe or a script, goes to the unchanged `Cli::parse()` path,
//! so help, errors and exit codes stay byte for byte what they were (I6).

use std::ffi::OsString;
use std::io::IsTerminal;

use anyhow::Result;
use clap::Parser;

use super::event_loop;
use super::exit_print::print_lines;
use super::icons::IconMode;
use super::layout::{Geometry, MIN_HEIGHT, MIN_WIDTH};
use super::settings::{resolve, settings_path};
use super::theme::Theme;

/// Exit status when the terminal is too small, as for a usage error.
const TOO_SMALL_STATUS: i32 = 2;

/// What `main` does next.
#[derive(Debug, PartialEq, Eq)]
pub enum Launch {
    /// Not a TUI launch: parse the command line as before.
    NotRequested,
    /// The TUI ran (or refused to); exit with this status.
    Done(i32),
}

/// The flags bare `sift` accepts.
#[derive(Parser, Debug)]
struct TuiArgs {
    /// Glyphs to draw, remembered for later runs
    #[arg(long, value_enum)]
    icons: IconMode,
}

/// Opens the TUI when this invocation asks for it in a real terminal.
pub fn launch() -> Result<Launch> {
    let args: Vec<OsString> = std::env::args_os().collect();
    if !asks_for_tui(&args) || !std::io::stdin().is_terminal() || !std::io::stdout().is_terminal() {
        return Ok(Launch::NotRequested);
    }
    let name = typed_name(&args);
    // A bad value exits here with clap's usual error and status 2.
    let requested = (args.len() > 1).then(|| TuiArgs::parse_from(&args).icons);
    let setting = resolve(settings_path().as_deref(), requested);
    let (width, height) = crossterm::terminal::size()?;
    if !Geometry::fits(width, height) {
        eprintln!("{}", too_small_line(&name, width, height));
        print_notices(setting.notice.iter());
        return Ok(Launch::Done(TOO_SMALL_STATUS));
    }
    let theme = Theme::from_env();
    let end = event_loop::run(setting.mode, theme)?;
    print_lines(&mut std::io::stdout().lock(), &end.exit_card, &theme)?;
    print_notices(end.notices.iter().chain(setting.notice.iter()));
    Ok(Launch::Done(0))
}

/// No arguments, or only `--icons <set>` / `--icons=<set>`.
fn asks_for_tui(args: &[OsString]) -> bool {
    match args.get(1).and_then(|arg| arg.to_str()) {
        None => args.len() <= 1,
        Some(first) => first == "--icons" || first.starts_with("--icons="),
    }
}

/// The name the user typed (`sift` or `tinysift`), for messages.
fn typed_name(args: &[OsString]) -> String {
    args.first()
        .map(std::path::Path::new)
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| "sift".into())
}

fn too_small_line(name: &str, width: u16, height: u16) -> String {
    format!(
        "{name} needs a terminal of at least {MIN_WIDTH}×{MIN_HEIGHT}; this one is {width}×{height}. \
         Make it larger, or run {name} <command> ({name} --help lists them)."
    )
}

fn print_notices<'a>(notices: impl Iterator<Item = &'a String>) {
    for notice in notices {
        eprintln!("{notice}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(list: &[&str]) -> Vec<OsString> {
        list.iter().map(OsString::from).collect()
    }

    #[test]
    fn only_a_bare_call_or_icons_asks_for_the_tui() {
        assert!(asks_for_tui(&args(&["sift"])));
        assert!(asks_for_tui(&args(&["sift", "--icons", "nerd"])));
        assert!(asks_for_tui(&args(&["sift", "--icons=unicode"])));
        for other in [
            &["sift", "clean"][..],
            &["sift", "--help"],
            &["sift", "--version"],
            &["sift", "-h"],
            &["sift", "--iconsx"],
            &["sift", "sys", "--icons", "nerd"],
        ] {
            assert!(!asks_for_tui(&args(other)), "{other:?}");
        }
    }

    #[test]
    fn the_flag_parses_both_sets() {
        let parsed = TuiArgs::try_parse_from(args(&["sift", "--icons", "nerd"])).unwrap();
        assert_eq!(parsed.icons, IconMode::Nerd);
        let parsed = TuiArgs::try_parse_from(args(&["sift", "--icons=unicode"])).unwrap();
        assert_eq!(parsed.icons, IconMode::Unicode);
        assert!(TuiArgs::try_parse_from(args(&["sift", "--icons", "emoji"])).is_err());
        assert!(TuiArgs::try_parse_from(args(&["sift", "--icons", "nerd", "clean"])).is_err());
    }

    #[test]
    fn the_size_line_is_one_line_naming_the_minimum_and_the_commands() {
        let line = too_small_line("sift", 70, 20);
        assert!(!line.contains('\n'));
        assert!(line.contains("80×24") && line.contains("70×20"));
        assert!(line.contains("sift <command>"));
        assert!(too_small_line("tinysift", 70, 20).contains("tinysift <command>"));
    }

    #[test]
    fn the_typed_name_is_the_file_name() {
        assert_eq!(typed_name(&args(&["/opt/homebrew/bin/sift"])), "sift");
        assert_eq!(typed_name(&args(&["tinysift"])), "tinysift");
        assert_eq!(typed_name(&[]), "sift");
    }
}
