//! Counted words and short lists of names, worded one way on every page.

use super::size_format::format_count;
use super::text_fit::cut;

/// `1 category`, `5 categories`, `1,284 paths`, `2 apps`.
pub fn plural(count: usize, one: &str, many: &str) -> String {
    let word = if count == 1 { one } else { many };
    format!("{} {word}", format_count(count as u64))
}

/// Up to `shown` names, each cut to `width`, then `and N more`:
/// `Xcode DerivedData, pnpm store and 2 more`.
pub fn some_names(names: &[&str], shown: usize, width: usize) -> String {
    let listed = names
        .iter()
        .take(shown)
        .map(|name| cut(name, width))
        .collect::<Vec<_>>()
        .join(", ");
    match names.len().saturating_sub(shown) {
        0 => listed,
        more => format!("{listed} and {more} more"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counts_and_names_read_as_the_mock() {
        assert_eq!(plural(1, "category", "categories"), "1 category");
        assert_eq!(plural(1284, "path", "paths"), "1,284 paths");
        let names = [
            "Xcode DerivedData",
            "node_modules (idle)",
            "pnpm store",
            "a",
            "b",
        ];
        assert_eq!(
            some_names(&names, 3, 28),
            "Xcode DerivedData, node_modules (idle), pnpm store and 2 more"
        );
        assert_eq!(
            some_names(&names[..2], 3, 28),
            "Xcode DerivedData, node_modules (idle)"
        );
        assert_eq!(some_names(&["Photoshop"], 3, 5), "Phot…");
    }
}
