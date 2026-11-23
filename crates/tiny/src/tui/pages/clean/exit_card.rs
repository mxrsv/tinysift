//! The framed result printed once the TUI closes (TU4): the session's
//! cleans added up, Clean only (I24). A session with no clean prints nothing.

use crate::tui::styled_line::StyledLine;
use crate::tui::widgets::overlay_box::Overlay;
use crate::tui::widgets::size_format::format_size;
use crate::tui::widgets::text_fit::cut;

use super::run::Session;
use super::wording::seconds;

const CARD_WIDTH: usize = 60;
const NAME_WIDTH: usize = 36;
const SIZE_WIDTH: usize = 12;
/// Categories named in the card; the rest are counted.
const SHOWN: usize = 5;

pub fn exit_card(session: &Session) -> Option<Vec<StyledLine>> {
    if session.cleans == 0 {
        return None;
    }
    let totals = session.totals;
    let rows = session.rows.iter().take(SHOWN).map(|(_, label, bytes)| {
        StyledLine::new()
            .plain(format!("  {:<NAME_WIDTH$}", cut(label, NAME_WIDTH)))
            .dim(format!("{:>SIZE_WIDTH$}", format_size(*bytes)))
    });
    let more = session.rows.len().saturating_sub(SHOWN);
    let lines = [
        StyledLine::new()
            .ok("✓ ")
            .hi(format!("Cleaned {}", format_size(totals.cleaned)))
            .dim(format!("  in {}", seconds(session.took))),
        StyledLine::new(),
    ]
    .into_iter()
    .chain(rows)
    .chain((more > 0).then(|| StyledLine::new().faint(format!("  … {more} more"))))
    .chain([
        StyledLine::new(),
        StyledLine::new().dim(format!(
            "  In Trash {} · returns when the Trash is emptied",
            format_size(totals.in_trash)
        )),
    ])
    .chain(
        (totals.for_good > 0)
            .then(|| StyledLine::new().dim(format!("  For good {}", format_size(totals.for_good)))),
    )
    .chain((totals.kept > 0).then(|| {
        StyledLine::new().warn("● ").dim(format!(
            "Kept {} · run sift to see why",
            format_size(totals.kept)
        ))
    }))
    .collect();
    let card = Overlay {
        title: "TinySift".into(),
        lines,
    };
    Some(
        std::iter::once(StyledLine::new())
            .chain(card.boxed(CARD_WIDTH))
            .collect(),
    )
}
