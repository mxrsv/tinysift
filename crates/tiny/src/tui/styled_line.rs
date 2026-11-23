//! One line of terminal text as a list of role-tagged segments.
//!
//! Pages describe what they draw with roles, never with colours: the frame
//! resolves each [`Role`] through the [`Theme`](super::theme::Theme) when it
//! paints, so `NO_COLOR` and the selected row are handled in one place.
//! Widths are display cells measured per grapheme, exactly as ratatui
//! measures what it draws, so a wide character, an emoji with a variation
//! selector or a joined emoji never pushes a row past the terminal's edge.

use ratatui::buffer::CellWidth;
use unicode_segmentation::UnicodeSegmentation;

/// What a piece of text means; the theme decides how it looks.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Role {
    /// Ordinary text in the terminal's own foreground.
    #[default]
    Plain,
    /// A value the eye should hit: bold.
    Hi,
    /// A label beside a value.
    Dim,
    /// Decoration: rules, separators, track of a meter, "… N more".
    Faint,
    /// Done, finished, nothing to do.
    Ok,
    /// Review, warning, kept.
    Warn,
    /// Permanent, failed, refused.
    Bad,
    /// Storage bars and figures.
    Storage,
    /// CPU bars.
    Cpu,
    /// Memory bars.
    Memory,
}

/// A run of text drawn with one role. `selected` draws it as the highlighted row or button.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Seg {
    pub role: Role,
    pub text: String,
    pub selected: bool,
}

/// A line built from segments: `StyledLine::new().hi("6.2 GB").dim(" picked")`.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct StyledLine {
    segs: Vec<Seg>,
}

/// The graphemes of `text` with the cells each takes as ratatui draws it: a
/// grapheme holding a control character draws nothing.
pub fn graphemes(text: &str) -> impl Iterator<Item = (&str, usize)> {
    text.graphemes(true).map(|grapheme| {
        let cells = if grapheme.contains(char::is_control) {
            0
        } else {
            usize::from(grapheme.cell_width())
        };
        (grapheme, cells)
    })
}

/// Display width of a string in terminal cells.
pub fn text_width(text: &str) -> usize {
    graphemes(text).map(|(_, cells)| cells).sum()
}

macro_rules! role_builders {
    ($($name:ident => $role:ident),* $(,)?) => {
        $(
            #[doc = concat!("Appends `text` drawn as [`Role::", stringify!($role), "`].")]
            pub fn $name(self, text: impl Into<String>) -> Self {
                self.push(Role::$role, text)
            }
        )*
    };
}

impl StyledLine {
    pub fn new() -> Self {
        Self::default()
    }

    /// A line holding one segment.
    pub fn of(role: Role, text: impl Into<String>) -> Self {
        Self::new().push(role, text)
    }

    /// Appends a segment; empty text adds nothing.
    pub fn push(self, role: Role, text: impl Into<String>) -> Self {
        let text = text.into();
        if text.is_empty() {
            return self;
        }
        let mut segs = self.segs;
        segs.push(Seg {
            role,
            text,
            selected: false,
        });
        Self { segs }
    }

    role_builders!(plain => Plain, hi => Hi, dim => Dim, faint => Faint, bad => Bad);

    /// Appends every segment of `other`, keeping its roles and selection.
    pub fn append(self, other: StyledLine) -> Self {
        Self {
            segs: self.segs.into_iter().chain(other.segs).collect(),
        }
    }

    /// The same line drawn as the highlighted row (or a focused button).
    pub fn selected(self) -> Self {
        Self {
            segs: self
                .segs
                .into_iter()
                .map(|seg| Seg {
                    selected: true,
                    ..seg
                })
                .collect(),
        }
    }

    pub fn segs(&self) -> &[Seg] {
        &self.segs
    }

    /// The text without roles.
    #[cfg(test)]
    pub fn text(&self) -> String {
        self.segs.iter().map(|seg| seg.text.as_str()).collect()
    }

    pub fn width(&self) -> usize {
        self.segs.iter().map(|seg| text_width(&seg.text)).sum()
    }

    /// At most `width` cells; a line that is cut ends in a faint `…`.
    pub fn clip(self, width: usize) -> Self {
        if self.width() <= width {
            return self;
        }
        if width == 0 {
            return Self::new();
        }
        let selected = self.segs.last().is_some_and(|seg| seg.selected);
        let kept = self.take_cells(width - 1);
        let tail = Seg {
            role: Role::Faint,
            text: "…".into(),
            selected,
        };
        let mut segs = kept.segs;
        segs.push(tail);
        // A wide character that did not fit leaves a gap: pad it.
        Self { segs }.pad(width)
    }

    /// Pads with plain spaces to `width` cells; a wider line is unchanged.
    pub fn pad(self, width: usize) -> Self {
        let missing = width.saturating_sub(self.width());
        if missing == 0 {
            return self;
        }
        let selected = self.segs.last().is_some_and(|seg| seg.selected);
        let mut segs = self.segs;
        segs.push(Seg {
            role: Role::Plain,
            text: " ".repeat(missing),
            selected,
        });
        Self { segs }
    }

    /// Exactly `width` cells: clipped with `…` or padded with spaces.
    pub fn fit(self, width: usize) -> Self {
        self.clip(width).pad(width)
    }

    /// The first `cells` cells, never splitting a wide character.
    fn take_cells(self, cells: usize) -> Self {
        let mut left = cells;
        let mut segs = Vec::new();
        for seg in self.segs {
            if left == 0 {
                break;
            }
            let mut text = String::new();
            for (grapheme, w) in graphemes(&seg.text) {
                if w > left {
                    left = 0;
                    break;
                }
                left -= w;
                text.push_str(grapheme);
            }
            if !text.is_empty() {
                segs.push(Seg { text, ..seg });
            }
        }
        Self { segs }
    }
}

/// The state and data roles, which only pages draw.
impl StyledLine {
    role_builders!(ok => Ok, warn => Warn, storage => Storage);
}

impl From<&str> for StyledLine {
    fn from(text: &str) -> Self {
        Self::of(Role::Plain, text)
    }
}

impl From<String> for StyledLine {
    fn from(text: String) -> Self {
        Self::of(Role::Plain, text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builders_keep_roles_in_order_and_skip_empty_text() {
        let line = StyledLine::new().hi("6.2 GB").dim("").dim(" picked");
        let roles: Vec<_> = line.segs().iter().map(|s| s.role).collect();
        assert_eq!(roles, [Role::Hi, Role::Dim]);
        assert_eq!(line.text(), "6.2 GB picked");
    }

    #[test]
    fn clip_ends_in_a_faint_ellipsis_at_the_exact_width() {
        let line = StyledLine::new()
            .hi("Crash reports / DiagnosticReports")
            .clip(10);
        assert_eq!(line.text(), "Crash rep…");
        assert_eq!(line.width(), 10);
        assert_eq!(line.segs().last().unwrap().role, Role::Faint);
    }

    #[test]
    fn fit_pads_short_lines_and_leaves_exact_ones() {
        assert_eq!(StyledLine::from("ab").fit(4).text(), "ab  ");
        assert_eq!(StyledLine::from("abcd").fit(4).text(), "abcd");
        assert_eq!(StyledLine::from("abcd").fit(0).text(), "");
    }

    #[test]
    fn wide_characters_never_overflow() {
        let line = StyledLine::from("ab漢字").fit(4);
        assert_eq!(line.width(), 4);
        assert_eq!(line.text(), "ab… ");
    }

    #[test]
    fn width_is_counted_per_grapheme_as_ratatui_draws_it() {
        // A heart with the emoji selector, a joined family, CJK.
        assert_eq!(text_width("\u{2764}\u{FE0F}"), 2);
        assert_eq!(text_width("\u{1F469}\u{200D}\u{1F4BB}"), 2);
        assert_eq!(text_width("漢字"), 4);
        assert_eq!(text_width("e\u{301}"), 1);
        let line = StyledLine::from("ab\u{1F469}\u{200D}\u{1F4BB}cd").fit(4);
        assert_eq!(line.width(), 4);
        assert_eq!(line.text(), "ab… ");
    }

    #[test]
    fn data_and_state_roles_have_builders() {
        let line = StyledLine::new().ok("✓").warn("review").storage("━");
        let roles: Vec<_> = line.segs().iter().map(|s| s.role).collect();
        assert_eq!(roles, [Role::Ok, Role::Warn, Role::Storage]);
    }

    #[test]
    fn selection_survives_padding() {
        let line = StyledLine::from("x").selected().fit(3);
        assert!(line.segs().iter().all(|s| s.selected));
    }
}
