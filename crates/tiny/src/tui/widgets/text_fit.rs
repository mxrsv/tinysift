//! Fitting text into a width: end ellipsis, middle ellipsis for paths, word
//! wrap, path wrap after a slash, and padding to a column. Widths are display
//! cells counted per grapheme, as ratatui counts them; never pad with
//! `format!("{:<w$}")`, which counts characters.

use crate::tui::styled_line::{graphemes, text_width};

/// At most `width` cells; cut text ends in `…`.
pub fn cut(text: &str, width: usize) -> String {
    if text_width(text) <= width {
        return text.into();
    }
    if width == 0 {
        return String::new();
    }
    format!("{}…", take_front(text, width - 1))
}

/// Exactly `width` cells: cut with `…`, then padded with spaces, so the
/// next column starts in the same place whatever the text holds.
pub fn fill(text: &str, width: usize) -> String {
    let cut = cut(text, width);
    let missing = width.saturating_sub(text_width(&cut));
    format!("{cut}{}", " ".repeat(missing))
}

/// At least `width` cells, the text at the right: a figure in a column.
pub fn fill_left(text: &str, width: usize) -> String {
    let missing = width.saturating_sub(text_width(text));
    format!("{}{text}", " ".repeat(missing))
}

/// At most `width` cells, keeping both ends: a third before the `…`, the rest
/// after it, so a path keeps its root and its file name.
pub fn middle_ellipsis(text: &str, width: usize) -> String {
    if text_width(text) <= width {
        return text.into();
    }
    if width == 0 {
        return String::new();
    }
    let front = (width - 1).div_ceil(3);
    let back = width - 1 - front;
    format!("{}…{}", take_front(text, front), take_back(text, back))
}

/// Greedy word wrap at spaces; a word wider than `width` is cut with `…`.
pub fn wrap(text: &str, width: usize) -> Vec<String> {
    join_greedy(text.split(' ').filter(|w| !w.is_empty()), " ", width)
}

/// Wraps a path after a slash, never inside a name. Measured as the mock
/// measures it: each slash counts one extra cell, so lines break a little early.
pub fn wrap_path(path: &str, width: usize) -> Vec<String> {
    let spaced = path.replace('/', "/ ");
    wrap(spaced.trim(), width)
        .into_iter()
        .map(|line| line.replace("/ ", "/"))
        .collect()
}

fn join_greedy<'a>(parts: impl Iterator<Item = &'a str>, gap: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for part in parts {
        if !line.is_empty() && text_width(&line) + text_width(gap) + text_width(part) > width {
            lines.push(cut(&line, width));
            line = String::new();
        }
        if !line.is_empty() {
            line.push_str(gap);
        }
        line.push_str(part);
    }
    if !line.is_empty() {
        lines.push(cut(&line, width));
    }
    lines
}

fn take_front(text: &str, cells: usize) -> String {
    let mut left = cells;
    graphemes(text)
        .take_while(|&(_, w)| {
            let fits = w <= left;
            left = left.saturating_sub(w);
            fits
        })
        .map(|(grapheme, _)| grapheme)
        .collect()
}

fn take_back(text: &str, cells: usize) -> String {
    let mut left = cells;
    let mut tail: Vec<&str> = graphemes(text)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .take_while(|&(_, w)| {
            let fits = w <= left;
            left = left.saturating_sub(w);
            fits
        })
        .map(|(grapheme, _)| grapheme)
        .collect();
    tail.reverse();
    tail.concat()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cut_keeps_short_text_and_ends_long_text_in_an_ellipsis() {
        assert_eq!(cut("pnpm store", 20), "pnpm store");
        assert_eq!(
            cut("Crash reports / DiagnosticReports", 15),
            "Crash reports …"
        );
        assert_eq!(cut("abc", 0), "");
    }

    #[test]
    fn fill_pads_to_cells_not_characters() {
        for name in [
            "漢字のアプリ",
            "Notes \u{2764}\u{FE0F}",
            "Dev \u{1F469}\u{200D}\u{1F4BB} kit",
            "plain",
        ] {
            assert_eq!(text_width(&fill(name, 12)), 12, "{name}");
            assert_eq!(text_width(&fill(name, 3)), 3, "{name}");
        }
        assert_eq!(fill_left("1 GB", 6), "  1 GB");
        assert_eq!(text_width(&fill_left("漢字", 6)), 6);
    }

    #[test]
    fn middle_ellipsis_matches_the_mock() {
        // mock.js `mid('~/Library/Developer/Xcode/DerivedData/TinySift-fmzkqpelrbdvxn', 43)`
        assert_eq!(
            middle_ellipsis(
                "~/Library/Developer/Xcode/DerivedData/TinySift-fmzkqpelrbdvxn",
                43
            ),
            "~/Library/Deve…Data/TinySift-fmzkqpelrbdvxn"
        );
        assert_eq!(middle_ellipsis("short", 43), "short");
        assert_eq!(text_width(&middle_ellipsis(&"x".repeat(200), 30)), 30);
    }

    #[test]
    fn wrap_breaks_at_spaces_and_cuts_words_that_never_fit() {
        assert_eq!(
            wrap("Each category is checked again right before it moves.", 20),
            [
                "Each category is",
                "checked again right",
                "before it moves."
            ]
        );
        assert_eq!(
            wrap("a supercalifragilistic word", 8),
            ["a", "superca…", "word"]
        );
        assert!(wrap("", 10).is_empty());
    }

    #[test]
    fn wrap_path_breaks_after_a_slash() {
        assert_eq!(
            wrap_path("~/Code/client-portal-2025/node_modules", 20),
            ["~/Code/", "client-portal-2025/", "node_modules"]
        );
    }
}
