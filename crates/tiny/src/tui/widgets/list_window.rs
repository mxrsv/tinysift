//! Which rows of a long list fit the window, keeping the cursor visible.
//!
//! When rows remain below, the last visible row is spent on `… N more ↓`
//! (drawn by the frame); on the last page every row is a list row.

/// The rows the frame draws: `start..start + shown`, then `more_below`
/// hidden rows announced on the next line when it is not zero.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Window {
    pub start: usize,
    pub shown: usize,
    pub more_below: usize,
}

/// Scrolls as little as possible from `offset` so `cursor` stays in view.
pub fn window(len: usize, height: usize, cursor: Option<usize>, offset: usize) -> Window {
    if len <= height {
        return Window {
            start: 0,
            shown: len,
            more_below: 0,
        };
    }
    let cursor = cursor.map(|c| c.min(len - 1));
    if height < 2 {
        let start = cursor.unwrap_or(offset).min(len - height);
        return Window {
            start,
            shown: height,
            more_below: 0,
        };
    }
    let last_start = len - height;
    let mut start = offset.min(last_start);
    if let Some(cursor) = cursor {
        if cursor < start {
            start = cursor;
        } else if start < last_start && cursor > start + height - 2 {
            start = (cursor + 2 - height).min(last_start);
        }
    }
    if start == last_start {
        Window {
            start,
            shown: height,
            more_below: 0,
        }
    } else {
        Window {
            start,
            shown: height - 1,
            more_below: len - start - (height - 1),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_list_is_shown_whole() {
        assert_eq!(
            window(3, 10, Some(2), 0),
            Window {
                start: 0,
                shown: 3,
                more_below: 0
            }
        );
    }

    #[test]
    fn a_long_list_spends_its_last_row_on_the_count_like_the_mock() {
        // mock.js `win`: n - 1 rows, then `… (len - n + 1) more ↓`.
        assert_eq!(
            window(85, 20, Some(0), 0),
            Window {
                start: 0,
                shown: 19,
                more_below: 66
            }
        );
    }

    #[test]
    fn moving_down_scrolls_one_row_at_a_time() {
        let w = window(85, 20, Some(19), 0);
        assert_eq!(
            w,
            Window {
                start: 1,
                shown: 19,
                more_below: 65
            }
        );
        assert!(w.start <= 19 && 19 < w.start + w.shown);
    }

    #[test]
    fn the_last_page_has_no_count_and_shows_every_row() {
        assert_eq!(
            window(85, 20, Some(84), 0),
            Window {
                start: 65,
                shown: 20,
                more_below: 0
            }
        );
    }

    #[test]
    fn moving_up_above_the_window_scrolls_back() {
        assert_eq!(window(85, 20, Some(10), 40).start, 10);
    }

    #[test]
    fn a_shrunken_list_clamps_a_stale_offset() {
        assert_eq!(
            window(30, 20, Some(29), 80),
            Window {
                start: 10,
                shown: 20,
                more_below: 0
            }
        );
    }

    #[test]
    fn the_cursor_is_visible_for_every_position_and_height() {
        for len in 1..40 {
            for height in 1..12 {
                for cursor in 0..len {
                    let w = window(len, height, Some(cursor), 0);
                    assert!(
                        w.start <= cursor && cursor < w.start + w.shown,
                        "{len} {height} {cursor}"
                    );
                    let used = w.shown + usize::from(w.more_below > 0);
                    assert!(used <= height);
                    if height >= 2 {
                        assert_eq!(w.start + w.shown + w.more_below, len);
                    }
                }
            }
        }
    }
}
