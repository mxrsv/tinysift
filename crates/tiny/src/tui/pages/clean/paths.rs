//! The path list of one category: every path ticked until space unticks it.
//! Unticked paths go to the engine as excluded paths, as `sift clean`'s
//! drill-down does; the engine keeps them and any picked folder above them.

use crossterm::event::{KeyCode, KeyEvent};

use crate::tui::pages::{Detail, KeyHint, PageView, ViewCtx};
use crate::tui::styled_line::StyledLine;
use crate::tui::widgets::list_cursor::ListCursor;
use crate::tui::widgets::size_format::format_size;
use crate::tui::widgets::text_fit::{middle_ellipsis, wrap, wrap_path};

use super::listing::Listing;
use super::wording::{facts, plural, shown_path};

const NOTE: &str = "Unticking keeps this path and any picked folder above it.";
/// Cells a path row spends besides the path: lead, tick, gap and size.
const ROW_CHROME: usize = 15;
const FACT_LABEL: usize = 14;

pub struct PathList {
    /// The listing entry whose paths these are.
    pub entry: usize,
    /// Item indexes, largest first.
    order: Vec<usize>,
    cursor: ListCursor,
}

pub enum PathsReply {
    Stay,
    Back,
    /// Not the path list's key.
    Ignored,
}

impl PathList {
    pub fn open(listing: &Listing, entry: usize) -> Option<Self> {
        let group = listing.entries.get(entry)?.group()?;
        let mut order: Vec<usize> = (0..group.items.len()).collect();
        order.sort_by_key(|&i| std::cmp::Reverse(group.items[i].size));
        Some(Self {
            entry,
            order,
            cursor: ListCursor::default(),
        })
    }

    pub fn on_key(&mut self, key: &KeyEvent, listing: &mut Listing, rows: usize) -> PathsReply {
        if self.cursor.on_key(key, self.order.len(), rows, |_| true) {
            return PathsReply::Stay;
        }
        match key.code {
            KeyCode::Esc => PathsReply::Back,
            KeyCode::Char(' ') => {
                if let Some(path) = self.path_at(listing, self.cursor.index()) {
                    if !listing.excluded.remove(&path) {
                        listing.excluded.insert(path);
                    }
                }
                PathsReply::Stay
            }
            _ => PathsReply::Ignored,
        }
    }

    fn path_at(&self, listing: &Listing, row: usize) -> Option<std::path::PathBuf> {
        let group = listing.entries.get(self.entry)?.group()?;
        Some(group.items.get(*self.order.get(row)?)?.path.clone())
    }

    pub fn view(&self, listing: &Listing, ctx: &ViewCtx) -> PageView {
        let Some(entry) = listing.entries.get(self.entry) else {
            return PageView::default();
        };
        let Some(group) = entry.group() else {
            return PageView::default();
        };
        let width = ctx.list_width.saturating_sub(ROW_CHROME);
        let home = listing.home.as_ref();
        let ticked = |path: &std::path::PathBuf| !listing.excluded.contains(path);
        let list = self
            .order
            .iter()
            .enumerate()
            .map(|(row, &i)| {
                let item = &group.items[i];
                let here = row == self.cursor.index();
                let on = ticked(&item.path);
                let shown = middle_ellipsis(&shown_path(&item.path, home), width);
                let lead = if here {
                    StyledLine::new().hi("❯ ")
                } else {
                    StyledLine::from("  ")
                };
                let tick = if on {
                    StyledLine::new().hi("◉ ")
                } else {
                    StyledLine::new().faint("◯ ")
                };
                let name = format!("{shown:<width$}");
                let name = match (here, on) {
                    (true, _) => StyledLine::new().hi(name),
                    (false, true) => StyledLine::from(name),
                    (false, false) => StyledLine::new().dim(name),
                };
                lead.append(tick)
                    .append(name)
                    .plain(" ")
                    .dim(format!("{:>10}", format_size(item.size)))
            })
            .collect();
        let kept: Vec<_> = group.items.iter().filter(|i| ticked(&i.path)).collect();
        let kept_bytes = kept.iter().fold(0u64, |s, i| s.saturating_add(i.size));
        let header = vec![
            StyledLine::new().dim(format!(
                "{} · {} · {}",
                entry.category.label,
                plural(group.items.len(), "path", "paths"),
                format_size(group.total_size)
            )),
            StyledLine::new(),
        ];
        let count = crate::tui::widgets::size_format::format_count;
        let highlighted_on = self
            .path_at(listing, self.cursor.index())
            .is_none_or(|path| ticked(&path));
        PageView {
            header,
            list,
            cursor: (!self.order.is_empty()).then_some(self.cursor.index()),
            detail: self.detail(listing, ctx.detail_width),
            foot: Some(
                StyledLine::new()
                    .hi(format!(
                        "{} of {}",
                        count(kept.len() as u64),
                        count(group.items.len() as u64)
                    ))
                    .dim(" paths ticked    ")
                    .hi(format_size(kept_bytes)),
            ),
            keys: vec![
                KeyHint::new("↑↓", "move"),
                KeyHint::new("space", if highlighted_on { "untick" } else { "tick" }),
                KeyHint::new("esc", "back to categories"),
            ],
            ..PageView::default()
        }
    }

    fn detail(&self, listing: &Listing, width: usize) -> Option<Detail> {
        let group = listing.entries.get(self.entry)?.group()?;
        let item = group.items.get(*self.order.get(self.cursor.index())?)?;
        let path = shown_path(&item.path, listing.home.as_ref());
        let size = format_size(item.size);
        let facts = facts(&item.evidence, listing.read_at);
        let full = wrap_path(&path, width)
            .into_iter()
            .map(|line| StyledLine::new().hi(line))
            .chain([StyledLine::new().dim(size.as_str()), StyledLine::new()])
            .chain(facts.iter().map(|(label, value)| {
                StyledLine::new()
                    .dim(format!("{label:<FACT_LABEL$}"))
                    .plain(value.as_str())
            }))
            .chain((!facts.is_empty()).then(StyledLine::new))
            .chain(
                wrap(NOTE, width)
                    .into_iter()
                    .map(|l| StyledLine::new().dim(l)),
            )
            .collect();
        let brief = vec![
            StyledLine::new()
                .hi(middle_ellipsis(&path, width.saturating_sub(12)))
                .dim(format!("  {size}")),
            StyledLine::new().dim(
                facts
                    .iter()
                    .map(|(label, value)| format!("{label} {value}"))
                    .collect::<Vec<_>>()
                    .join(" · "),
            ),
            StyledLine::new().dim(NOTE),
        ];
        Some(Detail {
            title: "Highlighted".into(),
            full,
            brief,
        })
    }
}
