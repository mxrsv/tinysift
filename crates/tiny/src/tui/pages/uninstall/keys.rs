//! What each key does in each phase. While a box is open every key is the
//! box's; during a run only esc is the page's, and `q` waits for the app in
//! progress (the frame asks through `on_quit_request`).

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use tiny_core::options::SortBy;

use super::boxes;
use super::engine::UninstallEngine;
use super::tasks::Msg;
use super::{Phase, UninstallPage};
use crate::tui::pages::{KeyReply, PageCtx};
use crate::tui::widgets::action_menu::MenuReply;
use crate::tui::widgets::confirm_box::ConfirmReply;
use crate::tui::widgets::list_cursor::ListCursor;

/// A cursor on row `index` of `len` rows.
pub fn cursor_at(index: usize, len: usize) -> ListCursor {
    let mut cursor = ListCursor::default();
    cursor.on_key(
        &KeyEvent::new(KeyCode::End, KeyModifiers::NONE),
        len,
        1,
        |_| true,
    );
    cursor.settle(len, |row| row == index);
    cursor
}

/// The command's `--sort` orders, in the order `s` steps through them.
fn next_sort(sort: &SortBy) -> SortBy {
    match sort {
        SortBy::LastUsed => SortBy::Size,
        SortBy::Size => SortBy::Name,
        SortBy::Name => SortBy::LastUsed,
    }
}

/// Keys the frame keeps for itself while the page is busy: pages and quit.
fn is_global(key: &KeyEvent) -> bool {
    matches!(
        key.code,
        KeyCode::Tab | KeyCode::BackTab | KeyCode::Char('q' | '1'..='9')
    )
}

impl<E: UninstallEngine> UninstallPage<E> {
    pub(super) fn handle_key(
        &mut self,
        key: KeyEvent,
        ctx: &mut PageCtx<'_, Msg<E::Frozen>>,
    ) -> KeyReply {
        match &mut self.phase {
            Phase::Done(run) if key.code == KeyCode::Enter => {
                // The apps that ran were acted on; a refusal before the menu
                // ran nothing, so its picks stay.
                if !run.refused {
                    self.ticked.clear();
                }
                self.read(ctx);
                KeyReply::Handled
            }
            Phase::Reading { task, .. } if key.code == KeyCode::Esc => {
                task.cancel();
                self.phase = Phase::Stopped;
                KeyReply::Handled
            }
            Phase::Stopped | Phase::Unreadable(_) if key.code == KeyCode::Char('r') => {
                self.read(ctx);
                KeyReply::Handled
            }
            Phase::Browse => self.browse_key(key, ctx),
            Phase::Freezing { task, .. } if key.code == KeyCode::Esc => {
                task.cancel();
                self.back_to_list(ctx);
                KeyReply::Handled
            }
            Phase::Menu { menu, .. } => {
                match menu.on_key(&key) {
                    MenuReply::Open => {}
                    MenuReply::Cancelled => self.back_to_list(ctx),
                    MenuReply::Chosen(choice) => self.choose(choice, ctx),
                }
                KeyReply::Handled
            }
            Phase::Confirm { confirm, .. } => {
                match confirm.on_key(&key) {
                    ConfirmReply::Open => {}
                    ConfirmReply::Cancelled => self.back_to_list(ctx),
                    ConfirmReply::Confirmed => {
                        if let Phase::Confirm { batch, .. } =
                            std::mem::replace(&mut self.phase, Phase::Browse)
                        {
                            self.start_run(batch, true, ctx);
                        }
                    }
                }
                KeyReply::Handled
            }
            Phase::DryRun { batch, cursor } => {
                let rows = super::run_view::dry_run_rows(batch);
                if cursor.on_key(&key, rows, ctx.list_rows(), |_| true) {
                    return KeyReply::Handled;
                }
                if matches!(key.code, KeyCode::Enter | KeyCode::Esc) {
                    self.back_to_list(ctx);
                    return KeyReply::Handled;
                }
                KeyReply::Ignored
            }
            Phase::Running { run, task } => {
                if key.code == KeyCode::Esc {
                    task.cancel();
                    run.stopping = true;
                }
                if is_global(&key) {
                    KeyReply::Ignored
                } else {
                    KeyReply::Handled
                }
            }
            Phase::Freezing { .. } if !is_global(&key) => KeyReply::Handled,
            _ => KeyReply::Ignored,
        }
    }

    fn browse_key(&mut self, key: KeyEvent, ctx: &mut PageCtx<'_, Msg<E::Frozen>>) -> KeyReply {
        let len = self.apps.len();
        if self.cursor.on_key(&key, len, ctx.list_rows(), |_| true) {
            self.read_next_plan(ctx);
            return KeyReply::Handled;
        }
        match key.code {
            KeyCode::Char(' ') => {
                if let Some(row) = self.highlighted().filter(|row| !row.refused()) {
                    let path = row.app.path.clone();
                    if !self.ticked.remove(&path) {
                        self.ticked.insert(path);
                    }
                    self.read_next_plan(ctx);
                }
            }
            KeyCode::Char('s') => {
                self.sort = next_sort(&self.sort);
                self.resort();
            }
            KeyCode::Char('r') => self.read(ctx),
            KeyCode::Enter => self.freeze(ctx),
            _ => return KeyReply::Ignored,
        }
        KeyReply::Handled
    }

    /// A choice of the action menu: Move to Trash, Dry-run, Hard delete.
    fn choose(&mut self, choice: usize, ctx: &mut PageCtx<'_, Msg<E::Frozen>>) {
        let Phase::Menu { batch, .. } = std::mem::replace(&mut self.phase, Phase::Browse) else {
            return;
        };
        match boxes::Choice::from_index(choice) {
            Some(boxes::Choice::Trash) => self.start_run(batch, false, ctx),
            Some(boxes::Choice::DryRun) => {
                self.phase = Phase::DryRun {
                    batch,
                    cursor: ListCursor::default(),
                }
            }
            Some(boxes::Choice::Hard) => {
                let confirm = boxes::hard_delete(&batch);
                self.phase = Phase::Confirm { batch, confirm };
            }
            None => {}
        }
    }
}
