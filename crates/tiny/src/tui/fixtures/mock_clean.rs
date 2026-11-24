//! A test-only Clean page that draws the mock's normal data
//! (`DATA.normal.cats` in `tools/mock.js`) through the shared widgets. The
//! frame tests compare it with `tools/dump.js` output; page builders can read
//! it as a worked example of the contract.

use crate::tui::icons::IconMode;
use crate::tui::pages::{Detail, KeyHint, Page, PageId, PageView, ViewCtx};
use crate::tui::styled_line::{Role, StyledLine};
use crate::tui::widgets::action_menu::{ActionMenu, MenuChoice};
use crate::tui::widgets::confirm_box::{ConfirmBox, Tone};
use crate::tui::widgets::meter::meter;
use crate::tui::widgets::phrases::some_names;
use crate::tui::widgets::size_format::{format_count, format_size};
use crate::tui::widgets::text_fit::{cut, fill, middle_ellipsis, wrap};

struct Category {
    id: &'static str,
    name: &'static str,
    bytes: u64,
    review: bool,
    on: bool,
    tool: Option<&'static str>,
    items: u64,
    back: &'static str,
    why: &'static str,
    paths: &'static [&'static str],
}

/// `(id, name, bytes, review, ticked, items, comes back, why)`, as `cat()` in the mock.
type Row = (
    &'static str,
    &'static str,
    u64,
    bool,
    bool,
    u64,
    &'static str,
    &'static str,
);

const fn category((id, name, bytes, review, on, items, back, why): Row) -> Category {
    Category {
        id,
        name,
        bytes,
        review,
        on,
        tool: None,
        items,
        back,
        why,
        paths: &[],
    }
}

/// The mock's "normal" data (`DATA.normal.cats`), ticks as after `v` and the user's picks.
fn categories() -> Vec<Category> {
    vec![
        Category {
            paths: &[
                "~/Library/Developer/Xcode/DerivedData/TinySift-fmzkqpelrbdvxn",
                "~/Library/Developer/Xcode/DerivedData/ModuleCache.noindex",
            ],
            ..category((
                "xcode-derived",
                "Xcode DerivedData",
                6_200_000_000,
                false,
                true,
                43,
                "when you build again",
                "Xcode rebuilds it",
            ))
        },
        category((
            "node-modules",
            "node_modules (idle)",
            3_800_000_000,
            true,
            true,
            12,
            "on the next install",
            "12 projects idle 30+ days",
        )),
        category((
            "npm",
            "npm cache",
            1_100_000_000,
            true,
            false,
            1,
            "downloaded when needed",
            "npm fetches packages again",
        )),
        Category {
            tool: Some("pnpm store prune"),
            ..category((
                "pnpm",
                "pnpm store",
                840_000_000,
                true,
                true,
                1,
                "downloaded when needed",
                "entries nothing references",
            ))
        },
        Category {
            tool: Some("brew cleanup --prune=30"),
            ..category((
                "homebrew",
                "Homebrew cleanup",
                310_000_000,
                true,
                false,
                1,
                "downloaded when needed",
                "old versions, downloads 30+ days old",
            ))
        },
        category((
            "crash-reports",
            "Crash reports / DiagnosticReports",
            89_000_000,
            false,
            true,
            89,
            "on its own",
            "macOS writes new reports",
        )),
        category((
            "user-logs",
            "User logs",
            50_000,
            false,
            true,
            9,
            "on its own",
            "apps write new logs",
        )),
    ]
}

pub enum Boxed {
    None,
    Action,
    Hard,
}

pub struct MockClean {
    pub review: bool,
    pub boxed: Boxed,
}

impl MockClean {
    fn shown(&self) -> Vec<Category> {
        categories()
            .into_iter()
            .filter(|c| self.review || !c.review)
            .collect()
    }

    fn picked(&self) -> Vec<Category> {
        self.shown().into_iter().filter(|c| c.on).collect()
    }

    fn row(&self, index: usize, c: &Category, ctx: &ViewCtx, max: u64) -> StyledLine {
        let name_width = ctx.list_width - 39 - if ctx.icons == IconMode::Nerd { 2 } else { 0 };
        let name = fill(c.name, name_width);
        let size = format!("{:>10}", format_size(c.bytes));
        let lead = if index == 0 {
            StyledLine::new().hi("❯ ")
        } else {
            StyledLine::from("  ")
        };
        let tick = if c.on {
            StyledLine::new().hi("◉ ")
        } else {
            StyledLine::new().faint("◯ ")
        };
        let line = lead
            .append(tick)
            .dim(ctx.icons.category_prefix(c.id))
            .push(if c.on { Role::Hi } else { Role::Plain }, name)
            .plain(" ")
            .append(meter(c.bytes as f64 / max as f64, 12, Role::Storage))
            .plain(" ")
            .push(if c.on { Role::Hi } else { Role::Dim }, size)
            .plain("  ");
        if c.review {
            line.warn("review")
        } else {
            line
        }
    }

    fn detail(c: &Category, width: usize) -> Detail {
        let what = format!("{} · {} items", format_size(c.bytes), format_count(c.items));
        let goes = StyledLine::new()
            .dim("Goes to     ")
            .hi("Trash")
            .dim("  Finder › Put Back");
        let full = wrap(c.name, width)
            .into_iter()
            .map(|l| StyledLine::new().hi(l))
            .chain([
                StyledLine::new().dim(what.as_str()),
                StyledLine::new(),
                goes.clone(),
                StyledLine::new().dim("Comes back  ").plain(c.back),
            ])
            .chain(
                wrap(c.why, width - 12)
                    .into_iter()
                    .map(|l| StyledLine::new().dim("Listed      ").plain(l)),
            )
            .chain(std::iter::once(StyledLine::new()))
            .chain(
                c.paths
                    .iter()
                    .map(|p| StyledLine::new().dim(middle_ellipsis(p, width))),
            )
            .chain(std::iter::once(StyledLine::new().faint(format!(
                "… {} more · p lists them",
                format_count(c.items - c.paths.len() as u64)
            ))))
            .collect();
        let brief = vec![
            StyledLine::new()
                .hi(cut(c.name, width - 30))
                .dim(format!("  {what}")),
            goes.dim("    comes back ").plain(c.back),
            StyledLine::new().dim("Listed      ").plain(c.why),
        ];
        Detail {
            title: "Highlighted".into(),
            full,
            brief,
        }
    }

    fn foot(&self) -> StyledLine {
        let picked = self.picked();
        let total: u64 = picked.iter().map(|c| c.bytes).sum();
        let gone: u64 = picked
            .iter()
            .filter(|c| c.tool.is_some())
            .map(|c| c.bytes)
            .sum();
        let line = StyledLine::new()
            .hi(format_size(total))
            .dim(format!(" picked in {} categories    ", picked.len()))
            .ok("● ")
            .dim(format!("Trash {}", format_size(total - gone)));
        if gone > 0 {
            line.dim("    ")
                .bad("● ")
                .dim(format!("for good {}", format_size(gone)))
        } else {
            line
        }
    }
}

impl Page for MockClean {
    const ID: PageId = PageId::Clean;
    type Msg = ();

    fn view(&self, ctx: &ViewCtx) -> PageView {
        let shown = self.shown();
        let max = shown.iter().map(|c| c.bytes).max().unwrap_or(1);
        let more = (!self.review).then(|| {
            [
                StyledLine::new(),
                StyledLine::new().dim("    v also lists what is worth a look first"),
            ]
        });
        let list = shown
            .iter()
            .enumerate()
            .map(|(i, c)| self.row(i, c, ctx, max))
            .chain(more.into_iter().flatten())
            .collect();
        let swap = if self.review { "safe only" } else { "review" };
        let picked = self.picked();
        let total: u64 = picked.iter().map(|c| c.bytes).sum();
        let (keys, overlay) = match self.boxed {
            Boxed::None => (
                [
                    KeyHint::new("↑↓", "move"),
                    KeyHint::new("space", "tick"),
                    KeyHint::new("v", swap),
                    KeyHint::new("p", "paths"),
                    KeyHint::new("enter", "clean"),
                ]
                .into_iter()
                .chain(KeyHint::globals())
                .collect(),
                None,
            ),
            Boxed::Action => {
                let choice = |label: &str, description: &str, permanent| MenuChoice {
                    label: label.into(),
                    description: description.into(),
                    permanent,
                };
                let menu = ActionMenu::new(
                    format!("Clean {} categories · {}", picked.len(), format_size(total)),
                    vec![
                        choice("Move to Trash", "Put Back works in Finder", false),
                        choice("Dry-run", "change nothing, list the paths", false),
                        choice("Review paths", "untick single paths first", false),
                        choice("Hard delete", "skips the Trash, cannot be undone", true),
                    ],
                )
                .with_warning("pnpm store: no Trash, removed for good by any choice.");
                (ActionMenu::key_hints(), Some(menu.overlay()))
            }
            Boxed::Hard => {
                let items: u64 = picked.iter().map(|c| c.items).sum();
                let names: Vec<&str> = picked.iter().map(|c| c.name).collect();
                let names = some_names(&names, 3, 28);
                let facts = std::iter::once(StyledLine::from(format!(
                    "{} · {} items",
                    format_size(total),
                    items
                )))
                .chain(
                    wrap(&names, 50)
                        .into_iter()
                        .map(|l| StyledLine::new().dim(l)),
                )
                .chain(std::iter::once(
                    StyledLine::new().dim("It skips the Trash. Nothing can restore it."),
                ))
                .collect();
                let confirm = ConfirmBox::new(
                    "Hard delete",
                    Tone::Permanent,
                    format!("Delete {} categories for good?", picked.len()),
                    "Delete for good",
                )
                .with_facts(facts);
                (ConfirmBox::key_hints(), Some(confirm.overlay()))
            }
        };
        PageView {
            list,
            cursor: Some(0),
            detail: Some(Self::detail(&shown[0], ctx.detail_width)),
            foot: Some(self.foot()),
            keys,
            overlay,
            ..PageView::default()
        }
    }
}
