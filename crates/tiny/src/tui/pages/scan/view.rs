//! Scan's lines, drawn as the mock's `scan` screen and its states.

use std::path::{Path, PathBuf};

use tiny_core::options::ScanOptions;
use tiny_core::scan::types::FileEntry;

use crate::tui::pages::Detail;
use crate::tui::styled_line::{Role, StyledLine};
use crate::tui::widgets::size_format::{format_count, format_size};
use crate::tui::widgets::spinner::spinner_line;
use crate::tui::widgets::text_fit::{middle_ellipsis, wrap, wrap_path};

/// Cells before the path: lead (2), size (10), age (7), gap (2).
const BEFORE_PATH: usize = 21;
/// Detail labels (`Size      `) are this wide.
const LABEL: usize = 10;
const DAY: u64 = 86_400;
const READ_ONLY: &str = "Scan only reads. It never deletes or moves a file.";
/// The folders the command scans by default.
pub const DEFAULT_FOLDERS: &str = "Downloads, Desktop, Documents";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Which {
    Large,
    Old,
}

/// `100 MB and up · Downloads, Desktop, Documents`: the threshold of the shown list and the folders.
pub fn thresholds(which: Which, folders: &str) -> StyledLine {
    let defaults = ScanOptions::default();
    let threshold = match which {
        Which::Large => format!("{} MB and up", defaults.min_size_mb),
        Which::Old => format!("untouched {} days and up", defaults.older_than_days),
    };
    StyledLine::new().dim(format!("{threshold} · {folders}"))
}

/// The pinned switch: the shown list reversed, each with its count.
pub fn switch(which: Which, large: usize, old: usize) -> StyledLine {
    let tab = |name: &str, count: usize, on: bool| {
        let label = format!("{name} {}", format_count(count as u64));
        if on {
            StyledLine::new().plain(" ").hi(label).plain(" ").selected()
        } else {
            StyledLine::new().dim(format!(" {label} "))
        }
    };
    tab("Large", large, which == Which::Large)
        .plain(" ")
        .append(tab("Old", old, which == Which::Old))
}

pub fn idle() -> StyledLine {
    StyledLine::new()
        .hi("enter")
        .dim(" scans Downloads, Desktop and Documents. It only reads.")
}

pub fn scanning(text: Option<&str>, tick: u64) -> StyledLine {
    spinner_line(tick, format!("{}…", text.unwrap_or("Starting")))
}

/// A scan was asked for while the stopped one still walks.
pub fn waiting_for_stop(tick: u64) -> StyledLine {
    spinner_line(
        tick,
        "The previous scan is still stopping; this one starts when it ends…",
    )
}

pub fn nothing_found() -> Vec<StyledLine> {
    let defaults = ScanOptions::default();
    vec![
        StyledLine::new().ok("✓ ").hi("Nothing large or old here"),
        StyledLine::new().dim(format!(
            "  No file is {} MB or more, or untouched for {} days.",
            defaults.min_size_mb, defaults.older_than_days
        )),
    ]
}

pub fn nothing_in(which: Which) -> StyledLine {
    let name = match which {
        Which::Large => "Nothing large here",
        Which::Old => "Nothing old here",
    };
    StyledLine::new().ok("✓ ").hi(name)
}

/// The engine's error, wrapped, and why no list is shown.
pub fn failed(error: &str, width: usize) -> Vec<StyledLine> {
    wrap(error, width.saturating_sub(2))
        .into_iter()
        .enumerate()
        .map(|(i, line)| {
            let lead = if i == 0 {
                StyledLine::new().bad("● ")
            } else {
                StyledLine::from("  ")
            };
            lead.hi(line)
        })
        .chain(std::iter::once(StyledLine::new().dim(
            "  Nothing partial is shown. Allow access, then press r.",
        )))
        .collect()
}

pub fn row(file: &FileEntry, home: Option<&Path>, highlighted: bool, width: usize) -> StyledLine {
    let value = if highlighted { Role::Hi } else { Role::Plain };
    let lead = if highlighted {
        StyledLine::new().hi("❯ ")
    } else {
        StyledLine::from("  ")
    };
    let days = format!("{}d", format_count(file.age_secs / DAY));
    lead.push(value, format!("{:>10}", format_size(file.size)))
        .dim(format!("{days:>7}"))
        .plain("  ")
        .push(
            value,
            middle_ellipsis(
                &shown_path(&file.path, home),
                width.saturating_sub(BEFORE_PATH),
            ),
        )
}

pub fn detail(
    file: &FileEntry,
    home: Option<&Path>,
    reveal_error: Option<&str>,
    width: usize,
) -> Detail {
    let path = shown_path(&file.path, home);
    let size = format_size(file.size);
    let modified = age(file.age_secs);
    let error = reveal_error.map(|e| format!("Finder could not show it: {e}"));
    let full = wrap_path(&path, width)
        .into_iter()
        .map(|line| StyledLine::new().hi(line))
        .chain([
            StyledLine::new(),
            StyledLine::new()
                .dim(format!("{:<LABEL$}", "Size"))
                .plain(size.as_str()),
            StyledLine::new()
                .dim(format!("{:<LABEL$}", "Modified"))
                .plain(modified.as_str()),
            StyledLine::new(),
        ])
        .chain(
            wrap(READ_ONLY, width)
                .into_iter()
                .map(|l| StyledLine::new().dim(l)),
        )
        .chain(error.iter().flat_map(|text| {
            std::iter::once(StyledLine::new()).chain(
                wrap(text, width)
                    .into_iter()
                    .map(|l| StyledLine::new().push(Role::Bad, l)),
            )
        }))
        .collect();
    let last = match &error {
        Some(text) => StyledLine::new().bad("● ").plain(text.as_str()),
        None => StyledLine::new().dim(READ_ONLY),
    };
    let brief = vec![
        StyledLine::new().hi(middle_ellipsis(&path, width)),
        StyledLine::new()
            .dim("Size ")
            .plain(size)
            .dim("   modified ")
            .plain(modified),
        last,
    ];
    Detail {
        title: "Highlighted".into(),
        full,
        brief,
    }
}

/// The folder names a finished scan walked, or the defaults before one.
pub fn folders(roots: &[PathBuf]) -> String {
    if roots.is_empty() {
        return DEFAULT_FOLDERS.into();
    }
    roots
        .iter()
        .map(|root| {
            root.file_name().map_or_else(
                || root.display().to_string(),
                |n| n.to_string_lossy().into_owned(),
            )
        })
        .collect::<Vec<_>>()
        .join(", ")
}

/// `path` with the home folder written as `~`.
pub fn shown_path(path: &Path, home: Option<&Path>) -> String {
    match home.and_then(|home| path.strip_prefix(home).ok()) {
        Some(rest) if rest.as_os_str().is_empty() => "~".into(),
        Some(rest) => format!("~/{}", rest.display()),
        None => path.display().to_string(),
    }
}

/// The engine's error with the home folder written as `~`.
pub fn shown_error(error: &str, home: Option<&Path>) -> String {
    match home.map(|home| home.display().to_string()) {
        Some(home) if !home.is_empty() => error.replace(&format!("{home}/"), "~/"),
        _ => error.to_string(),
    }
}

fn age(seconds: u64) -> String {
    match seconds / DAY {
        0 => "today".into(),
        1 => "1 day ago".into(),
        days => format!("{} days ago", format_count(days)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn home_is_written_as_a_tilde_only_at_the_start() {
        let home = Path::new("/Users/someone");
        assert_eq!(
            shown_path(Path::new("/Users/someone/Downloads/x.iso"), Some(home)),
            "~/Downloads/x.iso"
        );
        assert_eq!(
            shown_path(Path::new("/Volumes/Users/someone/x"), Some(home)),
            "/Volumes/Users/someone/x"
        );
        assert_eq!(
            shown_error(
                "cannot read scan directory /Users/someone/Documents: denied",
                Some(home)
            ),
            "cannot read scan directory ~/Documents: denied"
        );
    }

    #[test]
    fn the_switch_reverses_the_shown_list() {
        let line = switch(Which::Old, 85, 15_882);
        assert_eq!(line.text(), " Large 85   Old 15,882 ");
        assert!(line.segs().last().unwrap().selected);
        assert!(!line.segs()[0].selected);
    }
}
