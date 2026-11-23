//! Roles to terminal styles, using only the terminal's own 16 ANSI colours.
//!
//! No background is ever forced. The highlighted row and tab are drawn
//! reversed and without colour, which reads on dark and light profiles alike;
//! a bright-black background was rejected because faint text (bright black)
//! vanishes on it and black text on it is hard to read on light profiles.
//! `NO_COLOR` keeps bold and dim but drops every colour.

use ratatui::style::{Color, Modifier, Style};

use super::styled_line::{Role, Seg};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Theme {
    colour: bool,
}

impl Theme {
    /// Colour unless `NO_COLOR` is set to a non-empty value (no-color.org).
    pub fn from_env() -> Self {
        let no_colour = std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty());
        Self { colour: !no_colour }
    }

    #[cfg(test)]
    pub fn coloured() -> Self {
        Self { colour: true }
    }

    #[cfg(test)]
    pub fn monochrome() -> Self {
        Self { colour: false }
    }

    /// The style of one segment.
    pub fn style(&self, seg: &Seg) -> Style {
        if seg.selected {
            return self.selected(seg.role);
        }
        let base = Style::default();
        match seg.role {
            Role::Plain => base,
            Role::Hi => base.add_modifier(Modifier::BOLD),
            Role::Dim => base.add_modifier(Modifier::DIM),
            Role::Faint if self.colour => base.fg(Color::DarkGray),
            Role::Faint => base.add_modifier(Modifier::DIM),
            Role::Ok => self.tint(base, Color::Green),
            Role::Warn => self.tint(base, Color::Yellow),
            Role::Bad => self.tint(base, Color::Red),
            Role::Storage => self.tint(base, Color::Blue),
            Role::Cpu => self.tint(base, Color::Cyan),
            Role::Memory => self.tint(base, Color::Magenta),
        }
    }

    /// The highlighted row: reversed, no colour, bold kept for values.
    fn selected(&self, role: Role) -> Style {
        let style = Style::default().add_modifier(Modifier::REVERSED);
        if role == Role::Hi {
            style.add_modifier(Modifier::BOLD)
        } else {
            style
        }
    }

    fn tint(&self, style: Style, colour: Color) -> Style {
        if self.colour {
            style.fg(colour)
        } else {
            style
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(role: Role, selected: bool) -> Seg {
        Seg {
            role,
            text: "x".into(),
            selected,
        }
    }

    #[test]
    fn roles_map_to_the_terminal_palette_without_backgrounds() {
        let theme = Theme::coloured();
        assert_eq!(theme.style(&seg(Role::Ok, false)).fg, Some(Color::Green));
        assert_eq!(theme.style(&seg(Role::Warn, false)).fg, Some(Color::Yellow));
        assert_eq!(theme.style(&seg(Role::Bad, false)).fg, Some(Color::Red));
        assert_eq!(
            theme.style(&seg(Role::Storage, false)).fg,
            Some(Color::Blue)
        );
        assert_eq!(theme.style(&seg(Role::Cpu, false)).fg, Some(Color::Cyan));
        assert_eq!(
            theme.style(&seg(Role::Memory, false)).fg,
            Some(Color::Magenta)
        );
        for role in [Role::Plain, Role::Hi, Role::Dim, Role::Faint, Role::Ok] {
            assert_eq!(theme.style(&seg(role, false)).bg, None);
        }
    }

    #[test]
    fn no_colour_keeps_weight_and_reverses_the_selection() {
        let theme = Theme::monochrome();
        for role in [Role::Ok, Role::Bad, Role::Storage, Role::Faint] {
            assert_eq!(theme.style(&seg(role, false)).fg, None);
        }
        let hi = theme.style(&seg(Role::Hi, false));
        assert!(hi.add_modifier.contains(Modifier::BOLD));
        let picked = theme.style(&seg(Role::Dim, true));
        assert!(picked.add_modifier.contains(Modifier::REVERSED));
        assert_eq!(picked.fg, None);
    }

    #[test]
    fn the_selected_row_drops_colour_on_every_theme() {
        let picked = Theme::coloured().style(&seg(Role::Storage, true));
        assert_eq!(picked.fg, None);
        assert_eq!(picked.bg, None);
        assert!(picked.add_modifier.contains(Modifier::REVERSED));
    }
}
