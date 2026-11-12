//! Spotlight's last-used date of a file (GR3): asked only for a file that
//! already looks old, through the bounded runner, and `None` wherever
//! Spotlight has no such date.

use std::path::Path;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use crate::runner::CommandRunner;

/// Absolute path: a Finder-launched app has a minimal `PATH`.
const MDLS: &str = "/usr/bin/mdls";

/// Where a last-used date comes from; tests answer from a table.
pub trait LastUsed {
    fn last_used(&self, path: &Path) -> Option<SystemTime>;
}

/// No Spotlight date at all: the modification date alone judges.
pub struct NoLastUsed;

impl LastUsed for NoLastUsed {
    fn last_used(&self, _path: &Path) -> Option<SystemTime> {
        None
    }
}

/// Reads `kMDItemLastUsedDate` with `mdls`.
pub struct SpotlightLastUsed<'r>(pub &'r dyn CommandRunner);

impl LastUsed for SpotlightLastUsed<'_> {
    fn last_used(&self, path: &Path) -> Option<SystemTime> {
        let out = self.0.run(
            MDLS,
            &[
                "-raw",
                "-name",
                "kMDItemLastUsedDate",
                &path.to_string_lossy(),
            ],
        );
        out.success.then(|| parse_mdls_date(&out.stdout)).flatten()
    }
}

/// `2026-11-01 08:30:00 +0000`; `(null)` and anything else is `None`.
pub(crate) fn parse_mdls_date(text: &str) -> Option<SystemTime> {
    let mut parts = text.split_whitespace();
    let (date, time, zone) = (parts.next()?, parts.next()?, parts.next()?);
    let mut d = date.split('-').map(|p| p.parse::<i64>().ok());
    let (year, month, day) = (d.next()??, d.next()??, d.next()??);
    let mut t = time.split(':').map(|p| p.parse::<i64>().ok());
    let (hour, minute, second) = (t.next()??, t.next()??, t.next()??);
    let sign = match zone.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let digits: i64 = zone.get(1..)?.parse().ok()?;
    let offset = sign * ((digits / 100) * 3600 + (digits % 100) * 60);
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    let seconds =
        days_from_civil(year, month, day) * 86_400 + hour * 3600 + minute * 60 + second - offset;
    let seconds = u64::try_from(seconds).ok()?;
    Some(UNIX_EPOCH + Duration::from_secs(seconds))
}

/// Days since 1970-01-01 of a proleptic Gregorian date (Hinnant).
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let y = if month <= 2 { year - 1 } else { year };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (month + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}
