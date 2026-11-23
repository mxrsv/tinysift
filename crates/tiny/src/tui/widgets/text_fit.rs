//! Fitting text into a width: end ellipsis, middle ellipsis for paths, word
//! wrap, and path wrap after a slash. Widths are display cells.

use crate::tui::styled_line::{char_width, text_width};

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
    text.chars()
        .take_while(|&ch| {
            let w = char_width(ch);
            let fits = w <= left;
            left = left.saturating_sub(w);
            fits
        })
        .collect()
}

fn take_back(text: &str, cells: usize) -> String {
    let mut left = cells;
    let mut tail: Vec<char> = text
        .chars()
        .rev()
        .take_while(|&ch| {
            let w = char_width(ch);
            let fits = w <= left;
            left = left.saturating_sub(w);
            fits
        })
        .collect();
    tail.reverse();
    tail.into_iter().collect()
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
