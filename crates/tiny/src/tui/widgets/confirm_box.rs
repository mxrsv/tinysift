//! The confirmation box: a question, its facts and consequence, and two
//! buttons with the safe one focused. Hard delete (Clean, Uninstall) and
//! Quit / Force Quit (Processes) go through it.

use crossterm::event::{KeyCode, KeyEvent};

use super::overlay_box::{Overlay, MAX_BOX_WIDTH};
use super::text_fit::cut;
use crate::tui::pages::KeyHint;
use crate::tui::styled_line::{Role, StyledLine};

/// The question is cut short of the border, as the mock does.
const QUESTION_WIDTH: usize = MAX_BOX_WIDTH - 8;

/// The colour of the `●` before the question.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "opened by Clean, Uninstall and Processes, plan tasks 05, 07 and 09"
    )
)]
pub enum Tone {
    /// Orange: Quit.
    Warning,
    /// Red: Hard delete, Force Quit.
    Permanent,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConfirmReply {
    /// The key moved the focus or was ignored; the box stays open.
    Open,
    /// Enter on the confirming button. The only reply that may act.
    Confirmed,
    /// Esc, or enter on the safe button: change nothing.
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(
    not(test),
    allow(
        dead_code,
        reason = "opened by Clean, Uninstall and Processes, plan tasks 05, 07 and 09"
    )
)]
pub struct ConfirmBox {
    title: String,
    tone: Tone,
    question: String,
    facts: Vec<StyledLine>,
    safe_label: String,
    act_label: String,
    act_focused: bool,
}

#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "opened by Clean, Uninstall and Processes, plan tasks 05, 07 and 09"
    )
)]
impl ConfirmBox {
    /// A box with the safe button (`Cancel`) focused.
    pub fn new(
        title: impl Into<String>,
        tone: Tone,
        question: impl Into<String>,
        act_label: impl Into<String>,
    ) -> Self {
        Self {
            title: title.into(),
            tone,
            question: question.into(),
            facts: Vec::new(),
            safe_label: "Cancel".into(),
            act_label: act_label.into(),
            act_focused: false,
        }
    }

    /// Lines under the question, indented two cells: the count and size, the
    /// names, and last the consequence (style each as the screen needs).
    pub fn with_facts(self, facts: Vec<StyledLine>) -> Self {
        Self { facts, ..self }
    }

    /// ←→ or tab move the focus; enter answers for the focused button; esc cancels.
    pub fn on_key(&mut self, key: &KeyEvent) -> ConfirmReply {
        match key.code {
            KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::BackTab => {
                self.act_focused = !self.act_focused;
                ConfirmReply::Open
            }
            KeyCode::Enter if self.act_focused => ConfirmReply::Confirmed,
            KeyCode::Enter | KeyCode::Esc => ConfirmReply::Cancelled,
            _ => ConfirmReply::Open,
        }
    }

    /// The key bar while the box is open.
    pub fn key_hints() -> Vec<KeyHint> {
        vec![
            KeyHint::new("←→", "move"),
            KeyHint::new("enter", "choose"),
            KeyHint::new("esc", "cancel"),
        ]
    }

    pub fn overlay(&self) -> Overlay {
        let dot = match self.tone {
            Tone::Warning => Role::Warn,
            Tone::Permanent => Role::Bad,
        };
        let question = StyledLine::new()
            .push(dot, "● ")
            .hi(cut(&self.question, QUESTION_WIDTH));
        let facts = self
            .facts
            .iter()
            .map(|fact| StyledLine::from("  ").append(fact.clone()));
        let lines = [StyledLine::new(), question]
            .into_iter()
            .chain(facts)
            .chain([StyledLine::new(), self.buttons(), StyledLine::new()])
            .collect();
        Overlay {
            title: self.title.clone(),
            lines,
        }
    }

    fn buttons(&self) -> StyledLine {
        let button = |label: &str, focused: bool| {
            if focused {
                StyledLine::new().plain(" ").hi(label).plain(" ").selected()
            } else {
                StyledLine::new().plain(" ").dim(label).plain(" ")
            }
        };
        // The mock draws the unfocused act button without its leading space;
        // both forms keep the label in the same column.
        let gap = if self.act_focused { "   " } else { "    " };
        let act = if self.act_focused {
            button(&self.act_label, true)
        } else {
            StyledLine::new().dim(self.act_label.clone())
        };
        button(&self.safe_label, !self.act_focused)
            .plain(gap)
            .append(act)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn hard_delete() -> ConfirmBox {
        ConfirmBox::new(
            "Hard delete",
            Tone::Permanent,
            "Delete 5 categories for good?",
            "Delete for good",
        )
        .with_facts(vec![
            StyledLine::from("10.9 GB · 154 items"),
            StyledLine::new().dim("Xcode DerivedData, node_modules (idle), pnpm store"),
            StyledLine::new().dim("and 2 more"),
            StyledLine::new().dim("It skips the Trash. Nothing can restore it."),
        ])
    }

    fn press(confirm: &mut ConfirmBox, code: KeyCode) -> ConfirmReply {
        confirm.on_key(&KeyEvent::new(code, KeyModifiers::NONE))
    }

    #[test]
    fn draws_the_mock_hard_delete_box() {
        let rows: Vec<String> = hard_delete()
            .overlay()
            .boxed(MAX_BOX_WIDTH)
            .iter()
            .map(|l| l.text())
            .collect();
        // node tools/dump.js hard (the box only)
        let expected = [
            "╭─ Hard delete ──────────────────────────────────────────╮",
            "│                                                        │",
            "│ ● Delete 5 categories for good?                        │",
            "│   10.9 GB · 154 items                                  │",
            "│   Xcode DerivedData, node_modules (idle), pnpm store   │",
            "│   and 2 more                                           │",
            "│   It skips the Trash. Nothing can restore it.          │",
            "│                                                        │",
            "│  Cancel     Delete for good                            │",
            "│                                                        │",
            "╰────────────────────────────────────────────────────────╯",
        ];
        assert_eq!(rows, expected);
    }

    #[test]
    fn draws_the_mock_quit_box_with_an_orange_dot() {
        let quit = ConfirmBox::new(
            "Quit process",
            Tone::Warning,
            "Quit Google Chrome Helper (Renderer)?",
            "Quit",
        )
        .with_facts(vec![
            StyledLine::new().dim("PID 17703 is asked to close. Unsaved work may be lost.")
        ]);
        let overlay = quit.overlay();
        assert_eq!(overlay.lines[1].segs()[0].role, Role::Warn);
        let rows: Vec<String> = overlay
            .boxed(MAX_BOX_WIDTH)
            .iter()
            .map(|l| l.text().trim_end().to_string())
            .collect();
        // node tools/dump.js quit,size=wide (the box only)
        assert_eq!(
            rows[2],
            "│ ● Quit Google Chrome Helper (Renderer)?                │"
        );
        // The mock cuts this fact too: a fact wider than the box ends in `…`.
        assert_eq!(
            rows[3],
            "│   PID 17703 is asked to close. Unsaved work may be los…│"
        );
        assert_eq!(
            rows[5],
            "│  Cancel     Quit                                       │"
        );
    }

    #[test]
    fn opens_on_cancel_so_enter_and_esc_change_nothing() {
        let mut confirm = hard_delete();
        assert_eq!(press(&mut confirm, KeyCode::Enter), ConfirmReply::Cancelled);
        assert_eq!(press(&mut confirm, KeyCode::Esc), ConfirmReply::Cancelled);
        assert_eq!(press(&mut confirm, KeyCode::Char('y')), ConfirmReply::Open);
        assert_eq!(press(&mut confirm, KeyCode::Enter), ConfirmReply::Cancelled);
    }

    #[test]
    fn only_enter_on_the_moved_focus_confirms() {
        let mut confirm = hard_delete();
        assert_eq!(press(&mut confirm, KeyCode::Right), ConfirmReply::Open);
        let buttons = confirm.overlay().lines[7].clone();
        assert_eq!(buttons.text(), " Cancel     Delete for good ");
        assert!(buttons.segs().last().unwrap().selected);
        assert_eq!(press(&mut confirm, KeyCode::Enter), ConfirmReply::Confirmed);
        press(&mut confirm, KeyCode::Left);
        assert_eq!(press(&mut confirm, KeyCode::Enter), ConfirmReply::Cancelled);
    }
}
