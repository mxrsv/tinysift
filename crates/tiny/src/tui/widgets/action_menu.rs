//! The action box: a vertical list of choices, each with a description, and
//! an optional warning line. Clean and Uninstall open it on enter.

use crossterm::event::{KeyCode, KeyEvent};

use super::overlay_box::{Overlay, MAX_BOX_WIDTH};
use super::text_fit::wrap;
use crate::tui::pages::KeyHint;
use crate::tui::styled_line::{Role, StyledLine};

/// The highlighted row fills the box but its last cell.
const ROW_WIDTH: usize = MAX_BOX_WIDTH - 4;
/// The warning wraps short of the border, as the mock does.
const WARNING_WIDTH: usize = MAX_BOX_WIDTH - 8;

/// One choice. `permanent` draws its description red (Hard delete).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MenuChoice {
    pub label: String,
    pub description: String,
    pub permanent: bool,
}

/// What a key did to the menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MenuReply {
    /// The key was used or ignored; the menu stays open.
    Open,
    /// Enter on the choice at this index.
    Chosen(usize),
    /// Esc: close without acting.
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionMenu {
    title: String,
    choices: Vec<MenuChoice>,
    facts: Vec<StyledLine>,
    warning: Option<String>,
    selected: usize,
}

impl ActionMenu {
    /// A menu with the first choice highlighted.
    pub fn new(title: impl Into<String>, choices: Vec<MenuChoice>) -> Self {
        Self {
            title: title.into(),
            choices,
            facts: Vec::new(),
            warning: None,
            selected: 0,
        }
    }

    /// Lines under the choices that say what the choices act on, e.g. how
    /// much of the total is the app, its cache and its data.
    pub fn with_facts(self, facts: Vec<StyledLine>) -> Self {
        Self { facts, ..self }
    }

    /// A red `●` line under the choices, e.g. "pnpm store: no Trash, removed for good by any choice."
    pub fn with_warning(self, warning: impl Into<String>) -> Self {
        Self {
            warning: Some(warning.into()),
            ..self
        }
    }

    #[cfg(test)]
    pub fn selected(&self) -> usize {
        self.selected
    }

    /// ↑↓ move, enter chooses, esc cancels; every other key is swallowed.
    pub fn on_key(&mut self, key: &KeyEvent) -> MenuReply {
        match key.code {
            KeyCode::Up => self.selected = self.selected.saturating_sub(1),
            KeyCode::Down => {
                self.selected = (self.selected + 1).min(self.choices.len().saturating_sub(1));
            }
            KeyCode::Enter if !self.choices.is_empty() => return MenuReply::Chosen(self.selected),
            KeyCode::Esc => return MenuReply::Cancelled,
            _ => {}
        }
        MenuReply::Open
    }

    /// The key bar while the menu is open.
    pub fn key_hints() -> Vec<KeyHint> {
        vec![
            KeyHint::new("↑↓", "move"),
            KeyHint::new("enter", "choose"),
            KeyHint::new("esc", "cancel"),
        ]
    }

    pub fn overlay(&self) -> Overlay {
        let label_width = self
            .choices
            .iter()
            .map(|c| crate::tui::styled_line::text_width(&c.label))
            .max()
            .unwrap_or(0)
            + 2;
        let rows = self.choices.iter().enumerate().map(|(index, choice)| {
            let on = index == self.selected;
            let label = format!(
                "{}{}",
                choice.label,
                " ".repeat(
                    label_width.saturating_sub(crate::tui::styled_line::text_width(&choice.label))
                )
            );
            let description_role = if choice.permanent {
                Role::Bad
            } else {
                Role::Dim
            };
            let row = if on {
                StyledLine::new().hi("❯ ").hi(label)
            } else {
                StyledLine::new().plain("  ").plain(label)
            }
            .push(description_role, choice.description.clone());
            if on {
                row.fit(ROW_WIDTH).selected()
            } else {
                row
            }
        });
        let warning = self.warning.iter().flat_map(|text| {
            wrap(text, WARNING_WIDTH)
                .into_iter()
                .enumerate()
                .map(|(index, line)| {
                    let lead = if index == 0 {
                        StyledLine::new().bad("● ")
                    } else {
                        StyledLine::from("  ")
                    };
                    lead.dim(line)
                })
                .chain(std::iter::once(StyledLine::new()))
                .collect::<Vec<_>>()
        });
        let facts = (!self.facts.is_empty()).then(|| {
            self.facts
                .iter()
                .cloned()
                .chain(std::iter::once(StyledLine::new()))
        });
        let lines = std::iter::once(StyledLine::new())
            .chain(rows)
            .chain(std::iter::once(StyledLine::new()))
            .chain(facts.into_iter().flatten())
            .chain(warning)
            .collect();
        Overlay {
            title: self.title.clone(),
            lines,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn menu() -> ActionMenu {
        let choice = |label: &str, description: &str, permanent| MenuChoice {
            label: label.into(),
            description: description.into(),
            permanent,
        };
        ActionMenu::new(
            "Clean 5 categories · 10.9 GB",
            vec![
                choice("Move to Trash", "Put Back works in Finder", false),
                choice("Dry-run", "change nothing, list the paths", false),
                choice("Review paths", "untick single paths first", false),
                choice("Hard delete", "skips the Trash, cannot be undone", true),
            ],
        )
        .with_warning("pnpm store: no Trash, removed for good by any choice.")
    }

    fn press(menu: &mut ActionMenu, code: KeyCode) -> MenuReply {
        menu.on_key(&KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn draws_the_mock_action_box() {
        let rows: Vec<String> = menu()
            .overlay()
            .boxed(MAX_BOX_WIDTH)
            .iter()
            .map(|l| l.text().trim_end().to_string())
            .collect();
        // node tools/dump.js action,size=wide (the box only)
        let expected = [
            "╭─ Clean 5 categories · 10.9 GB ─────────────────────────╮",
            "│                                                        │",
            "│ ❯ Move to Trash  Put Back works in Finder              │",
            "│   Dry-run        change nothing, list the paths        │",
            "│   Review paths   untick single paths first             │",
            "│   Hard delete    skips the Trash, cannot be undone     │",
            "│                                                        │",
            "│ ● pnpm store: no Trash, removed for good by any        │",
            "│   choice.                                              │",
            "│                                                        │",
            "╰────────────────────────────────────────────────────────╯",
        ];
        assert_eq!(rows, expected);
    }

    #[test]
    fn the_highlighted_choice_is_selected_and_hard_delete_is_red() {
        let overlay = menu().overlay();
        assert!(overlay.lines[1].segs().iter().all(|s| s.selected));
        assert!(overlay.lines[4].segs().iter().any(|s| s.role == Role::Bad));
    }

    #[test]
    fn keys_move_choose_and_cancel() {
        let mut menu = menu();
        assert_eq!(press(&mut menu, KeyCode::Up), MenuReply::Open);
        assert_eq!(press(&mut menu, KeyCode::Down), MenuReply::Open);
        assert_eq!(press(&mut menu, KeyCode::Char('q')), MenuReply::Open);
        assert_eq!(press(&mut menu, KeyCode::Enter), MenuReply::Chosen(1));
        for _ in 0..10 {
            press(&mut menu, KeyCode::Down);
        }
        assert_eq!(menu.selected(), 3);
        assert_eq!(press(&mut menu, KeyCode::Esc), MenuReply::Cancelled);
    }
}
