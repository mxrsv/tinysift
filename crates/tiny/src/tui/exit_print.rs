//! Printing styled lines to the normal screen after the TUI has closed, so
//! the exit card stays in the scrollback (TU4).

use std::io::{self, Write};

use crossterm::queue;
use crossterm::style::{PrintStyledContent, StyledContent};
use ratatui::backend::IntoCrossterm;

use super::styled_line::StyledLine;
use super::theme::Theme;

/// Writes each line with the theme's styles, ending every line with a newline.
pub fn print_lines(out: &mut impl Write, lines: &[StyledLine], theme: &Theme) -> io::Result<()> {
    for line in lines {
        for seg in line.segs() {
            let style = theme.style(seg).into_crossterm();
            queue!(
                out,
                PrintStyledContent(StyledContent::new(style, seg.text.as_str()))
            )?;
        }
        writeln!(out)?;
    }
    out.flush()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn colour_is_printed_unless_no_color_is_set() {
        let card = [StyledLine::new().ok("✓ ").hi("Cleaned 10.9 GB")];
        let mut coloured = Vec::new();
        print_lines(&mut coloured, &card, &Theme::coloured()).unwrap();
        let coloured = String::from_utf8(coloured).unwrap();
        assert!(
            coloured.contains("\x1b[38;5;2m") || coloured.contains("\x1b[32m"),
            "{coloured:?}"
        );
        assert!(coloured.ends_with('\n'));

        let mut plain = Vec::new();
        print_lines(&mut plain, &card, &Theme::monochrome()).unwrap();
        let plain = String::from_utf8(plain).unwrap();
        assert!(
            !plain.contains("38;5;") && !plain.contains("\x1b[32m"),
            "{plain:?}"
        );
        assert!(plain.contains("Cleaned 10.9 GB"));
    }

    #[test]
    fn nothing_is_printed_for_an_empty_card() {
        let mut out = Vec::new();
        print_lines(&mut out, &[], &Theme::coloured()).unwrap();
        assert!(out.is_empty());
    }
}
