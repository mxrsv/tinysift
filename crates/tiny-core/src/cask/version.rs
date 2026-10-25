//! Ordering the versions of an app and of its cask, so an update is never
//! offered, planned or run towards a version the app is already past.
//!
//! Homebrew's default `brew outdated` says a cask is outdated when the
//! version it recorded differs from the cask's (`!=`, not `<`). For an app
//! that updated itself and whose cask does not declare `auto_updates`, that
//! records an old version while the app on disk is newer than the cask. So
//! "outdated" is not enough: the direction has to be checked, and only on
//! what is known.
//!
//! **The rule** ([`compare`]). A version is a comma-separated list (Homebrew
//! keeps a build number after a comma: `0.4.26,4`). Lists are compared element
//! by element, and an element is its dot-separated segments:
//!
//! - equal text is equal;
//! - otherwise the segments' leading digits are compared as numbers (so
//!   `1.9` < `1.10` and `08` = `8`); the first difference decides;
//! - equal digits followed by different text (`3b` against `3`,
//!   `7-dbc92f97` against `7`), a segment with no leading digits, or lists of
//!   different lengths (`2.0` against `2.0,200`) cannot be ordered: the answer
//!   is `None`, never a guess;
//! - a longer element with only zero segments left over equals the shorter
//!   (`1.0` = `1.0.0`); any other leftover makes it greater.
//!
//! **What is done with the answer** ([`offers_update`]). An update from
//! `current` to `target` is offered only when the texts differ and `current`
//! is not known to be equal or newer. When the order is unknown the update is
//! kept (the build-number-only update `2.0,100` to `2.0,200`, or a version in
//! a scheme this cannot read), because hiding it would hide a real update;
//! when the app is known to be newer it is not, because it would be a
//! downgrade.

use std::cmp::Ordering;

/// Orders `a` against `b` by the rule in the module docs; `None` when it
/// cannot be told.
pub(super) fn compare(a: &str, b: &str) -> Option<Ordering> {
    let (a, b): (Vec<&str>, Vec<&str>) = (a.split(',').collect(), b.split(',').collect());
    if a.len() != b.len() {
        // `2.0` and `2.0,200`: the build of one of them is unknown. If the
        // elements they share already differ, that decides.
        let shared = a.len().min(b.len());
        return match compare_lists(&a[..shared], &b[..shared]) {
            Some(Ordering::Equal) | None => None,
            decided => decided,
        };
    }
    compare_lists(&a, &b)
}

fn compare_lists(a: &[&str], b: &[&str]) -> Option<Ordering> {
    for (x, y) in a.iter().zip(b) {
        match compare_element(x, y)? {
            Ordering::Equal => {}
            decided => return Some(decided),
        }
    }
    Some(Ordering::Equal)
}

fn compare_element(a: &str, b: &str) -> Option<Ordering> {
    let (a, b): (Vec<&str>, Vec<&str>) = (a.split('.').collect(), b.split('.').collect());
    for (x, y) in a.iter().zip(&b) {
        if x == y {
            continue;
        }
        let ((nx, rest_x), (ny, rest_y)) = (leading_number(x)?, leading_number(y)?);
        match nx.cmp(&ny) {
            Ordering::Equal if rest_x == rest_y => {}
            Ordering::Equal => return None,
            decided => return Some(decided),
        }
    }
    let (shorter, longer, flip) = if a.len() <= b.len() {
        (a.len(), &b, false)
    } else {
        (b.len(), &a, true)
    };
    // What the longer one has beyond the shared segments.
    let leftover = &longer[shorter..];
    let order = match leftover.iter().find(|segment| !is_zero(segment)) {
        None => return Some(Ordering::Equal),
        Some(segment) if leading_number(segment).is_some() => Ordering::Greater,
        Some(_) => return None,
    };
    Some(if flip { order } else { order.reverse() })
}

/// `7-dbc92f97` is `(7, "-dbc92f97")`; `beta` has no leading number.
fn leading_number(segment: &str) -> Option<(u64, &str)> {
    let end = segment
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(segment.len());
    let number = segment[..end].parse().ok()?;
    Some((number, &segment[end..]))
}

fn is_zero(segment: &str) -> bool {
    !segment.is_empty() && segment.bytes().all(|b| b == b'0')
}

/// Whether to offer updating an app at `current` to the cask's `target`.
/// Both are raw Homebrew version text.
pub(super) fn offers_update(current: &str, target: &str) -> bool {
    current != target
        && !matches!(
            compare(current, target),
            Some(Ordering::Equal | Ordering::Greater)
        )
}

#[cfg(test)]
mod tests;
