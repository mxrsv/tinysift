//! The one frame of the mock: tab bar, rule, body, rule, key bar.
//!
//! [`compose`] turns a [`PageView`] into rows exactly as wide as the terminal
//! (the port of `frame()` in the mock's `tools/mock.js`), and [`paint`] writes
//! them into a ratatui buffer through the theme. Every row is clipped with
//! `…` or padded, so no data can push a region out of place.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::{Line, Span};

use super::icons::IconMode;
use super::layout::{Geometry, MIN_HEIGHT, MIN_WIDTH};
use super::pages::{KeyHint, PageId, PageView};
use super::styled_line::{text_width, StyledLine};
use super::theme::Theme;
use super::widgets::list_window::window;
use super::widgets::overlay_box::Overlay;
use super::widgets::size_format::{format_count, format_size};

/// What the tab bar shows besides the tabs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TabBar {
    pub active: PageId,
    pub icons: IconMode,
    pub free_bytes: Option<u64>,
}

/// A composed screen: `rows` fill the terminal; `overlay` is drawn over them.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Composed {
    pub rows: Vec<StyledLine>,
    pub overlay: Option<PlacedBox>,
    /// The list's scroll offset after keeping the cursor in view.
    pub offset: usize,
    /// List rows that were visible, for PgUp/PgDn.
    pub list_rows: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlacedBox {
    pub x: usize,
    pub y: usize,
    pub rows: Vec<StyledLine>,
}

pub fn compose(geo: &Geometry, bar: &TabBar, view: &PageView, offset: usize) -> Composed {
    let width = geo.width;
    let rule = StyledLine::new().faint("─".repeat(width));
    let body = if geo.two_columns {
        two_column_body(geo, view, offset)
    } else {
        stacked_body(geo, view, offset)
    };
    let rows = std::iter::once(tab_bar(width, bar))
        .chain(std::iter::once(rule.clone()))
        .chain(body.rows)
        .chain(std::iter::once(rule))
        .chain(std::iter::once(key_bar(&view.keys)))
        .map(|row| row.fit(width))
        .collect();
    let overlay = view.overlay.as_ref().map(|overlay| {
        let box_width = Overlay::width_on(width);
        let boxed: Vec<_> = overlay
            .boxed(box_width)
            .into_iter()
            .take(geo.body_rows)
            .collect();
        PlacedBox {
            x: (width - box_width) / 2,
            y: 2 + geo.body_rows.saturating_sub(boxed.len()) / 2,
            rows: boxed,
        }
    });
    Composed {
        rows,
        overlay,
        offset: body.offset,
        list_rows: body.list_rows,
    }
}

/// The screen shown when the window shrinks below the minimum mid-session.
pub fn compose_too_small(width: u16, height: u16) -> Composed {
    let lines = [
        format!("sift needs at least {MIN_WIDTH}×{MIN_HEIGHT}"),
        format!("this window is {width}×{height}"),
        "q quits".to_string(),
    ];
    let (w, h) = (usize::from(width), usize::from(height));
    let top = h.saturating_sub(lines.len()) / 2;
    let rows = (0..h)
        .map(|y| {
            let text = y.checked_sub(top).and_then(|i| lines.get(i));
            text.map_or_else(StyledLine::new, |text| {
                let indent = w.saturating_sub(text_width(text)) / 2;
                StyledLine::new()
                    .plain(" ".repeat(indent))
                    .dim(text.as_str())
            })
            .fit(w)
        })
        .collect();
    Composed {
        rows,
        overlay: None,
        offset: 0,
        list_rows: 0,
    }
}

/// Writes a composed screen into `buf` at `area`.
pub fn paint(buf: &mut Buffer, area: Rect, composed: &Composed, theme: &Theme) {
    let draw = |buf: &mut Buffer, x: usize, y: usize, row: &StyledLine| {
        let (Ok(x), Ok(y)) = (u16::try_from(x), u16::try_from(y)) else {
            return;
        };
        if y >= area.height || x >= area.width {
            return;
        }
        let line = Line::from(
            row.segs()
                .iter()
                .map(|seg| Span::styled(seg.text.clone(), theme.style(seg)))
                .collect::<Vec<_>>(),
        );
        buf.set_line(area.x + x, area.y + y, &line, area.width - x);
    };
    for (y, row) in composed.rows.iter().enumerate() {
        draw(buf, 0, y, row);
    }
    if let Some(placed) = &composed.overlay {
        for (i, row) in placed.rows.iter().enumerate() {
            draw(buf, placed.x, placed.y + i, row);
        }
    }
}

fn tab_bar(width: usize, bar: &TabBar) -> StyledLine {
    let free = bar.free_bytes.map_or_else(StyledLine::new, |bytes| {
        StyledLine::new().dim(format!("{} free ", format_size(bytes)))
    });
    let strip = |gap: &str| {
        PageId::ALL
            .iter()
            .enumerate()
            .fold(StyledLine::new(), |line, (index, &page)| {
                let name = format!(" {}{} ", bar.icons.tab_prefix(page), page.title());
                let tab = if page == bar.active {
                    StyledLine::new().hi(name).selected()
                } else {
                    StyledLine::new().dim(name)
                };
                let line = if index > 0 { line.plain(gap) } else { line };
                line.append(tab)
            })
    };
    let spaced = strip(" ");
    let tabs = if spaced.width() + free.width() + 2 <= width {
        spaced
    } else {
        strip("")
    };
    let filler = width.saturating_sub(1 + tabs.width() + free.width()).max(1);
    StyledLine::from(" ")
        .append(tabs)
        .plain(" ".repeat(filler))
        .append(free)
}

fn key_bar(keys: &[KeyHint]) -> StyledLine {
    let text = keys
        .iter()
        .map(|hint| format!("{} {}", hint.key, hint.action))
        .collect::<Vec<_>>()
        .join(" · ");
    StyledLine::from(" ").dim(text)
}

struct Body {
    rows: Vec<StyledLine>,
    offset: usize,
    list_rows: usize,
}

fn foot_rows(view: &PageView) -> Vec<StyledLine> {
    view.foot.as_ref().map_or_else(Vec::new, |foot| {
        vec![
            StyledLine::new(),
            StyledLine::from(" ").append(foot.clone()),
        ]
    })
}

fn two_column_body(geo: &Geometry, view: &PageView, offset: usize) -> Body {
    let foot = foot_rows(view);
    let height = geo.body_rows.saturating_sub(foot.len());
    let list = list_area(view, height, geo.list_width, offset);
    let right: Vec<StyledLine> = view.detail.as_ref().map_or_else(Vec::new, |detail| {
        [
            StyledLine::new().dim(detail.title.as_str()),
            StyledLine::new(),
        ]
        .into_iter()
        .chain(detail.full.iter().cloned())
        .collect()
    });
    let separator = if view.detail.is_some() {
        StyledLine::new().faint(" │ ")
    } else {
        StyledLine::from("   ")
    };
    let rows = list
        .rows
        .into_iter()
        .enumerate()
        .map(|(i, row)| {
            StyledLine::from(" ")
                .append(row)
                .append(separator.clone())
                .append(
                    right
                        .get(i)
                        .cloned()
                        .unwrap_or_default()
                        .fit(geo.detail_width),
                )
        })
        .chain(foot)
        .collect();
    Body { rows, ..list }
}

fn stacked_body(geo: &Geometry, view: &PageView, offset: usize) -> Body {
    let foot = foot_rows(view);
    let band: Vec<StyledLine> = view.detail.as_ref().map_or_else(Vec::new, |detail| {
        let title_width = text_width(&detail.title);
        let rule = StyledLine::new()
            .faint("── ")
            .dim(detail.title.as_str())
            .faint(format!(
                " {}",
                "─".repeat(geo.list_width.saturating_sub(title_width + 4))
            ));
        let brief = if detail.brief.is_empty() {
            &detail.full
        } else {
            &detail.brief
        };
        std::iter::once(rule)
            .chain((0..3).map(|i| brief.get(i).cloned().unwrap_or_default()))
            .map(|line| line.fit(geo.list_width))
            .collect()
    });
    let height = geo.body_rows.saturating_sub(foot.len() + band.len());
    let list = list_area(view, height, geo.list_width, offset);
    let rows = list
        .rows
        .into_iter()
        .chain(band)
        .map(|row| StyledLine::from(" ").append(row))
        .chain(foot)
        .collect();
    Body { rows, ..list }
}

/// Pinned header rows, then the windowed list, then blank rows: `height` rows `width` wide.
fn list_area(view: &PageView, height: usize, width: usize, offset: usize) -> Body {
    let header: Vec<StyledLine> = view
        .header
        .iter()
        .take(height)
        .map(|line| line.clone().fit(width))
        .collect();
    let list_rows = height - header.len();
    let shown = window(view.list.len(), list_rows, view.cursor, offset);
    let visible = view.list[shown.start..shown.start + shown.shown]
        .iter()
        .enumerate()
        .map(|(i, line)| {
            let line = line.clone().fit(width);
            if view.cursor == Some(shown.start + i) {
                line.selected()
            } else {
                line
            }
        });
    let more = (shown.more_below > 0).then(|| {
        StyledLine::new()
            .faint(format!(
                "    … {} more ↓",
                format_count(shown.more_below as u64)
            ))
            .fit(width)
    });
    let rows: Vec<StyledLine> = header.into_iter().chain(visible).chain(more).collect();
    let blanks = height.saturating_sub(rows.len());
    Body {
        rows: rows
            .into_iter()
            .chain(std::iter::repeat_n(StyledLine::new().fit(width), blanks))
            .collect(),
        offset: shown.start,
        list_rows,
    }
}

#[cfg(test)]
#[path = "frame_tests.rs"]
mod tests;
