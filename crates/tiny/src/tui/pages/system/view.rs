//! System's rows and its "This Mac" detail, drawn as the mock's `system` screen.

use super::engine::{SystemReading, Volume};
use crate::tui::pages::Detail;
use crate::tui::styled_line::{text_width, Role, StyledLine};
use crate::tui::widgets::meter::meter;
use crate::tui::widgets::size_format::format_count;
use crate::tui::widgets::text_fit::{cut, middle_ellipsis};
use crate::util::format_duration;

/// The name column is at most this wide, as in the mock.
const NAME_COLUMN: usize = 28;
/// Cells after the bar: two spaces, the used figure (8), ` of … GB` (15) and the percent (5).
const FIGURES: usize = 31;
/// A meter turns to the warning role above this share in percent, and to the bad role above the next.
const WARN_ABOVE: u128 = 80;
const BAD_ABOVE: u128 = 92;
/// Labels in the right-hand pane are padded to this width.
const LABEL: usize = 8;

/// Disks in decimal units, as Finder and the tab bar's free figure count them.
const DECIMAL: u64 = 1_000;
/// Memory in binary units, as Activity Monitor and the Mac's spec sheet count it (24 GB).
const BINARY: u64 = 1_024;

pub fn rows(reading: &SystemReading, list_width: usize) -> Vec<StyledLine> {
    let layout = Columns::for_width(list_width);
    let memory = match reading.memory_used {
        Some(used) => layout.meter_row("In use", used, reading.memory_total, BINARY, Role::Memory),
        None => layout.unavailable_row("In use"),
    };
    let disks: Vec<StyledLine> = if reading.volumes.is_empty() {
        vec![StyledLine::new().dim("No disk could be read")]
    } else {
        reading
            .volumes
            .iter()
            .map(|Volume { name, used, total }| {
                layout.meter_row(name, *used, *total, DECIMAL, Role::Storage)
            })
            .collect()
    };
    [
        StyledLine::new().dim("MEMORY"),
        memory,
        StyledLine::new(),
        StyledLine::new().dim("DISKS"),
    ]
    .into_iter()
    .chain(disks)
    .collect()
}

pub fn detail(reading: &SystemReading, width: usize) -> Detail {
    let host = reading.host.clone().unwrap_or_else(|| "unavailable".into());
    let uptime = reading
        .uptime_seconds
        .map_or_else(|| "unavailable".into(), format_duration);
    let chip = if reading.chip.is_empty() {
        "unavailable".to_string()
    } else {
        reading.chip.clone()
    };
    let cores = reading.cores.to_string();
    let facts = [
        ("Host", host.as_str()),
        ("Uptime", uptime.as_str()),
        ("Chip", chip.as_str()),
        ("Cores", cores.as_str()),
    ];
    let full = facts
        .iter()
        .flat_map(|(label, value)| {
            chop(value, width.saturating_sub(LABEL))
                .into_iter()
                .enumerate()
                .map(move |(i, part)| {
                    let label = if i == 0 { *label } else { "" };
                    StyledLine::new()
                        .dim(format!("{label:<LABEL$}"))
                        .plain(part)
                })
        })
        .collect();
    let brief = vec![
        StyledLine::new()
            .dim("Host ")
            .plain(middle_ellipsis(&host, width.saturating_sub(5))),
        StyledLine::new().dim("Uptime ").plain(uptime),
        StyledLine::new()
            .dim("Chip ")
            .plain(chip)
            .dim("   cores ")
            .plain(cores),
    ];
    Detail {
        title: "This Mac".into(),
        full,
        brief,
    }
}

/// The name, bar and figure columns of a meter row at one list width.
struct Columns {
    name: usize,
    bar: usize,
}

impl Columns {
    fn for_width(list_width: usize) -> Self {
        let name = NAME_COLUMN.min(list_width.saturating_sub(44));
        Self {
            name,
            bar: list_width.saturating_sub(name + FIGURES),
        }
    }

    fn name_cell(&self, name: &str) -> String {
        pad_right(&cut(name, self.name.saturating_sub(1)), self.name)
    }

    fn meter_row(&self, name: &str, used: u64, total: u64, base: u64, tone: Role) -> StyledLine {
        let (unit, label) = unit_for(total, base);
        let share = |above: u128| u128::from(used) * 100 > u128::from(total) * above;
        let role = if share(BAD_ABOVE) {
            Role::Bad
        } else if share(WARN_ABOVE) {
            Role::Warn
        } else {
            tone
        };
        let fraction = if total == 0 {
            0.0
        } else {
            used as f64 / total as f64
        };
        StyledLine::new()
            .plain(self.name_cell(name))
            .append(meter(fraction, self.bar, role))
            .plain("  ")
            .hi(pad_left(&figure(used, unit), 8))
            .dim(pad_right(
                &format!(" of {} {label}", figure(total, unit)),
                15,
            ))
            .dim(pad_left(&format!("{}%", percent(used, total)), 5))
    }

    fn unavailable_row(&self, name: &str) -> StyledLine {
        StyledLine::new()
            .plain(self.name_cell(name))
            .append(meter(0.0, self.bar, Role::Memory))
            .plain("  ")
            .dim("unavailable")
    }
}

/// The unit a row's figures use, from its total: GB, else MB, else KB.
fn unit_for(total: u64, base: u64) -> (u64, &'static str) {
    let gb = base * base * base;
    let mb = base * base;
    if total >= gb {
        (gb, "GB")
    } else if total >= mb {
        (mb, "MB")
    } else {
        (base, "KB")
    }
}

/// `bytes` in `unit` to one decimal, half up; a whole number drops the `.0`
/// (`19.2`, `24`, `1,998.9`), as the mock prints it.
fn figure(bytes: u64, unit: u64) -> String {
    let tenths = (u128::from(bytes) * 10 + u128::from(unit) / 2) / u128::from(unit);
    let whole = u64::try_from(tenths / 10).unwrap_or(u64::MAX);
    match tenths % 10 {
        0 => format_count(whole),
        tenth => format!("{}.{tenth}", format_count(whole)),
    }
}

fn percent(used: u64, total: u64) -> u128 {
    if total == 0 {
        return 0;
    }
    (u128::from(used) * 200 + u128::from(total)) / (u128::from(total) * 2)
}

/// Hard chunks of `width` characters, as the mock wraps the host name.
fn chop(text: &str, width: usize) -> Vec<String> {
    let chars: Vec<char> = text.chars().collect();
    if chars.is_empty() || width == 0 {
        return vec![String::new()];
    }
    chars.chunks(width).map(|c| c.iter().collect()).collect()
}

fn pad_right(text: &str, width: usize) -> String {
    format!(
        "{text}{}",
        " ".repeat(width.saturating_sub(text_width(text)))
    )
}

fn pad_left(text: &str, width: usize) -> String {
    format!(
        "{}{text}",
        " ".repeat(width.saturating_sub(text_width(text)))
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn figures_match_the_mock() {
        const GIB: u64 = 1 << 30;
        assert_eq!(figure(24 * GIB, GIB), "24");
        assert_eq!(figure(19_200_000_000, 1_000_000_000), "19.2");
        assert_eq!(figure(1_998_900_000_000, 1_000_000_000), "1,998.9");
        assert_eq!(figure(8_000_000_000_000, 1_000_000_000), "8,000");
        assert_eq!(figure(0, 1_000_000_000), "0");
        assert_eq!(percent(431_600, 460_400), 94);
        assert_eq!(percent(1, 0), 0);
    }

    #[test]
    fn a_small_volume_is_counted_in_megabytes_not_as_zero_gigabytes() {
        assert_eq!(unit_for(23_340_000, DECIMAL), (1_000_000, "MB"));
        assert_eq!(figure(18_660_000, 1_000_000), "18.7");
        assert_eq!(unit_for(900, DECIMAL), (1_000, "KB"));
    }

    #[test]
    fn the_meter_warns_above_80_and_turns_bad_above_92_percent() {
        let columns = Columns::for_width(78);
        let bar_role = |used| {
            columns
                .meter_row("x", used, 100, DECIMAL, Role::Storage)
                .segs()
                .iter()
                .find(|seg| seg.text.contains('━'))
                .map(|seg| seg.role)
        };
        assert_eq!(bar_role(80), Some(Role::Storage));
        assert_eq!(bar_role(81), Some(Role::Warn));
        assert_eq!(bar_role(92), Some(Role::Warn));
        assert_eq!(bar_role(93), Some(Role::Bad));
    }
}
