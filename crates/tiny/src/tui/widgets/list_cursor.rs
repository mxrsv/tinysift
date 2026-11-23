//! The highlighted row of a page's list, moved by ↑↓, PgUp/PgDn, Home/End.
//!
//! A page keeps one `ListCursor` per list and reports `index()` as
//! [`PageView::cursor`](crate::tui::pages::PageView::cursor). Rows that cannot
//! be highlighted (a note, a blank line) are skipped through `selectable`.
//! The frame scrolls the window; the page only moves the index.

use crossterm::event::{KeyCode, KeyEvent};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ListCursor {
    index: usize,
}

impl ListCursor {
    pub fn index(&self) -> usize {
        self.index
    }

    /// Handles a movement key. `page_rows` is [`PageCtx::list_rows`](crate::tui::pages::PageCtx::list_rows).
    /// Returns `false` for any other key, so the page can treat it itself.
    pub fn on_key(
        &mut self,
        key: &KeyEvent,
        len: usize,
        page_rows: usize,
        selectable: impl Fn(usize) -> bool,
    ) -> bool {
        let page = page_rows.saturating_sub(1).max(1);
        let target = match key.code {
            KeyCode::Up => self.step(len, &selectable, false, 1),
            KeyCode::Down => self.step(len, &selectable, true, 1),
            KeyCode::PageUp => self.step(len, &selectable, false, page),
            KeyCode::PageDown => self.step(len, &selectable, true, page),
            KeyCode::Home => (0..len).find(|&i| selectable(i)),
            KeyCode::End => (0..len).rev().find(|&i| selectable(i)),
            _ => return false,
        };
        if let Some(index) = target {
            self.index = index;
        }
        true
    }

    /// Keeps the cursor on a selectable row after the list changed: the same
    /// index if possible, else the nearest selectable row above, else below.
    pub fn settle(&mut self, len: usize, selectable: impl Fn(usize) -> bool) {
        let start = self.index.min(len.saturating_sub(1));
        let found = (0..=start)
            .rev()
            .chain(start + 1..len)
            .find(|&i| i < len && selectable(i));
        self.index = found.unwrap_or(0);
    }

    /// Moves up to `steps` selectable rows; stops at the last one in reach.
    fn step(
        &self,
        len: usize,
        selectable: &impl Fn(usize) -> bool,
        down: bool,
        steps: usize,
    ) -> Option<usize> {
        let candidates: Box<dyn Iterator<Item = usize>> = if down {
            Box::new(self.index + 1..len)
        } else {
            Box::new((0..self.index.min(len)).rev())
        };
        candidates.filter(|&i| selectable(i)).take(steps).last()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    #[test]
    fn arrows_skip_rows_that_cannot_be_highlighted() {
        let mut cursor = ListCursor::default();
        let rows = |i: usize| i != 1 && i != 2;
        assert!(cursor.on_key(&key(KeyCode::Down), 5, 10, rows));
        assert_eq!(cursor.index(), 3);
        assert!(cursor.on_key(&key(KeyCode::Up), 5, 10, rows));
        assert_eq!(cursor.index(), 0);
        assert!(
            cursor.on_key(&key(KeyCode::Up), 5, 10, rows),
            "edge keys are still consumed"
        );
        assert_eq!(cursor.index(), 0);
    }

    #[test]
    fn page_keys_move_one_window_and_stop_at_the_ends() {
        let mut cursor = ListCursor::default();
        cursor.on_key(&key(KeyCode::PageDown), 100, 20, |_| true);
        assert_eq!(cursor.index(), 19);
        cursor.on_key(&key(KeyCode::End), 100, 20, |_| true);
        assert_eq!(cursor.index(), 99);
        cursor.on_key(&key(KeyCode::PageDown), 100, 20, |_| true);
        assert_eq!(cursor.index(), 99);
        cursor.on_key(&key(KeyCode::Home), 100, 20, |_| true);
        assert_eq!(cursor.index(), 0);
    }

    #[test]
    fn other_keys_are_left_to_the_page() {
        let mut cursor = ListCursor::default();
        assert!(!cursor.on_key(&key(KeyCode::Char(' ')), 5, 10, |_| true));
    }

    #[test]
    fn settle_keeps_the_cursor_on_a_real_row_after_a_reload() {
        let mut cursor = ListCursor::default();
        cursor.on_key(&key(KeyCode::End), 10, 10, |_| true);
        cursor.settle(4, |i| i != 3);
        assert_eq!(cursor.index(), 2);
        cursor.settle(0, |_| true);
        assert_eq!(cursor.index(), 0);
    }
}
