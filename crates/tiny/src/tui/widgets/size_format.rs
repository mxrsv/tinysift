//! Sizes and counts as the TUI prints them: `6.2 GB`, `89 MB`, `50 KB`,
//! `under 1 KB`, `0 B`, `1,998.9 GB`, `15,882`.
//!
//! Decimal units (1 GB = 1,000,000,000 bytes), as Finder and the mock use.
//! Rounding is half up on integers, so a figure never depends on float
//! formatting, and a size that rounds to 1,000 of a unit moves to the next
//! unit (`1 MB`, never `1000 KB`).

const KB: u64 = 1_000;
const MB: u64 = 1_000_000;
const TENTH_GB: u64 = 100_000_000;

/// A byte count with the unit that keeps it short.
pub fn format_size(bytes: u64) -> String {
    if bytes == 0 {
        return "0 B".into();
    }
    if bytes < KB {
        return "under 1 KB".into();
    }
    let kb = rounded(bytes, KB);
    if kb < 1_000 {
        return format!("{kb} KB");
    }
    let mb = rounded(bytes, MB);
    if mb < 1_000 {
        return format!("{mb} MB");
    }
    let tenths = rounded(bytes, TENTH_GB);
    format!("{}.{} GB", format_count(tenths / 10), tenths % 10)
}

/// An integer with thousands separators.
pub fn format_count(value: u64) -> String {
    let digits = value.to_string();
    let len = digits.len();
    digits
        .chars()
        .enumerate()
        .flat_map(|(index, digit)| {
            let comma = index > 0 && (len - index).is_multiple_of(3);
            comma
                .then_some(',')
                .into_iter()
                .chain(std::iter::once(digit))
        })
        .collect()
}

fn rounded(bytes: u64, unit: u64) -> u64 {
    bytes / unit + u64::from(bytes % unit >= unit.div_ceil(2))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_match_the_mock() {
        assert_eq!(format_size(6_200_000_000), "6.2 GB");
        assert_eq!(format_size(89_000_000), "89 MB");
        assert_eq!(format_size(50_000), "50 KB");
        assert_eq!(format_size(400), "under 1 KB");
        assert_eq!(format_size(0), "0 B");
        assert_eq!(format_size(1_998_900_000_000), "1,998.9 GB");
        assert_eq!(format_size(28_140_000), "28 MB");
    }

    #[test]
    fn rounding_is_half_up_and_never_prints_a_thousand_of_a_unit() {
        assert_eq!(format_size(999), "under 1 KB");
        assert_eq!(format_size(1_000), "1 KB");
        assert_eq!(format_size(1_500), "2 KB");
        assert_eq!(format_size(1_499), "1 KB");
        assert_eq!(format_size(999_499), "999 KB");
        assert_eq!(format_size(999_500), "1 MB");
        assert_eq!(format_size(999_499_999), "999 MB");
        assert_eq!(format_size(999_500_000), "1.0 GB");
        assert_eq!(format_size(6_250_000_000), "6.3 GB");
        assert_eq!(format_size(u64::MAX), "18,446,744,073.7 GB");
    }

    #[test]
    fn counts_use_thousands_separators() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1_000), "1,000");
        assert_eq!(format_count(15_882), "15,882");
        assert_eq!(format_count(1_203_884), "1,203,884");
    }
}
