//! What each key does on each Clean screen.

use crossterm::event::{KeyCode, KeyEvent};
use tiny_core::clean::types::CleanAction;

use super::boxes::{self, Choice, CHOICES};
use super::paths::{PathList, PathsReply};
use super::{CleanPage, Msg, Stage};
use crate::tui::pages::{KeyReply, PageCtx, PageId};
use crate::tui::widgets::action_menu::MenuReply;
use crate::tui::widgets::confirm_box::ConfirmReply;
use crate::tui::widgets::list_cursor::ListCursor;

impl CleanPage {
    pub(super) fn handle_key(&mut self, key: KeyEvent, ctx: &mut PageCtx<'_, Msg>) -> KeyReply {
        match &mut self.stage {
            Stage::Reading { .. } => {
                let len = self.listing.as_ref().map_or(0, |l| l.entries.len());
                if self.cursor.on_key(&key, len, ctx.list_rows(), |_| true) {
                    return KeyReply::Handled;
                }
                if key.code != KeyCode::Esc {
                    return KeyReply::Ignored;
                }
                self.cancel_read();
                self.listing = None;
                self.stage = Stage::Stopped;
                KeyReply::Handled
            }
            Stage::Stopped => self.on_reread_key(key, true, ctx),
            Stage::Listed => self.on_list_key(key, ctx),
            Stage::Paths(paths) => {
                let Some(listing) = &mut self.listing else {
                    return KeyReply::Ignored;
                };
                match paths.on_key(&key, listing, ctx.list_rows()) {
                    PathsReply::Stay => KeyReply::Handled,
                    PathsReply::Back => {
                        self.stage = Stage::Listed;
                        KeyReply::Handled
                    }
                    PathsReply::Ignored => KeyReply::Ignored,
                }
            }
            Stage::Menu(menu) => {
                match menu.on_key(&key) {
                    MenuReply::Open => {}
                    MenuReply::Cancelled => self.stage = Stage::Listed,
                    MenuReply::Chosen(index) => {
                        if let Some(&choice) = CHOICES.get(index) {
                            self.on_menu_choice(choice, ctx);
                        }
                    }
                }
                KeyReply::Handled
            }
            Stage::Confirm(confirm) => {
                match confirm.on_key(&key) {
                    ConfirmReply::Open => {}
                    ConfirmReply::Cancelled => self.stage = Stage::Listed,
                    ConfirmReply::Confirmed => self.start_run(CleanAction::HardDelete, ctx),
                }
                KeyReply::Handled
            }
            Stage::DryRun(cursor) => {
                let rows = self.listing.as_ref().map_or(0, boxes::dry_run_rows);
                if cursor.on_key(&key, rows, ctx.list_rows(), |_| true) {
                    return KeyReply::Handled;
                }
                if key.code == KeyCode::Esc {
                    self.stage = Stage::Listed;
                    return KeyReply::Handled;
                }
                KeyReply::Ignored
            }
            Stage::Running(_) => match key.code {
                KeyCode::Esc => {
                    self.stop_run();
                    KeyReply::Handled
                }
                // `q` reaches the frame, which asks every page to quit (I14).
                KeyCode::Char('q') => KeyReply::Ignored,
                _ => KeyReply::Handled,
            },
            Stage::Done(_) => {
                if key.code != KeyCode::Enter {
                    return KeyReply::Ignored;
                }
                ctx.refresh_free_space();
                ctx.reload(PageId::Clean);
                KeyReply::Handled
            }
        }
    }

    fn on_list_key(&mut self, key: KeyEvent, ctx: &mut PageCtx<'_, Msg>) -> KeyReply {
        let Some(listing) = &mut self.listing else {
            return self.on_reread_key(key, true, ctx);
        };
        let len = listing.entries.len();
        if self.cursor.on_key(&key, len, ctx.list_rows(), |_| true) {
            return KeyReply::Handled;
        }
        if listing.entries.is_empty() || listing.cleaning_off() {
            let offer_view = listing.view_switch_allowed(self.review);
            return self.on_reread_key(key, offer_view, ctx);
        }
        match key.code {
            KeyCode::Char(' ') => listing.toggle(self.cursor.index()),
            KeyCode::Char('p') => {
                if let Some(paths) = PathList::open(listing, self.cursor.index()) {
                    self.stage = Stage::Paths(paths);
                }
            }
            KeyCode::Enter if listing.picked().next().is_some() => {
                self.stage = Stage::Menu(boxes::action_menu(listing));
            }
            _ => return self.on_reread_key(key, true, ctx),
        }
        KeyReply::Handled
    }

    /// `v` and `r`, the keys that read again.
    fn on_reread_key(
        &mut self,
        key: KeyEvent,
        offer_view: bool,
        ctx: &mut PageCtx<'_, Msg>,
    ) -> KeyReply {
        match key.code {
            KeyCode::Char('v') if offer_view => {
                self.review = !self.review;
                self.start_read(ctx);
            }
            KeyCode::Char('r') => self.start_read(ctx),
            _ => return KeyReply::Ignored,
        }
        KeyReply::Handled
    }

    fn on_menu_choice(&mut self, choice: Choice, ctx: &mut PageCtx<'_, Msg>) {
        let Some(listing) = &self.listing else { return };
        self.stage = match choice {
            Choice::MoveToTrash => return self.start_run(CleanAction::Trash, ctx),
            Choice::DryRun => Stage::DryRun(ListCursor::default()),
            Choice::ReviewPaths => {
                let first_picked = listing
                    .entries
                    .iter()
                    .position(|e| e.ticked && e.tickable());
                let entry = Some(self.cursor.index())
                    .filter(|&i| listing.entries[i].ticked)
                    .or(first_picked);
                entry
                    .and_then(|i| PathList::open(listing, i))
                    .map_or(Stage::Listed, Stage::Paths)
            }
            Choice::HardDelete => Stage::Confirm(boxes::hard_delete_box(listing)),
        };
    }
}
