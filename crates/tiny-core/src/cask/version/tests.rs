use std::cmp::Ordering::{Equal, Greater, Less};

use super::*;

#[test]
fn numbers_are_ordered_as_numbers_segment_by_segment() {
    for (a, b, order) in [
        ("1.9", "1.10", Less),
        ("1.139.1", "1.142.0", Less),
        ("26.1013.40934", "26.1021.31345", Less),
        ("2027.08.28", "2026.10.13", Greater),
        ("3.0", "2.0", Greater),
        ("0.12.1", "0.12.1", Equal),
        ("08", "8", Equal),
        ("153.1.95.104", "1.97.59.0", Greater),
    ] {
        assert_eq!(compare(a, b), Some(order), "{a} against {b}");
        assert_eq!(compare(b, a), Some(order.reverse()), "{b} against {a}");
    }
}

#[test]
fn trailing_zero_segments_change_nothing_and_other_trailing_segments_make_it_greater() {
    for (a, b, order) in [
        ("1.0", "1.0.0", Equal),
        ("1.0", "1.0.0.0", Equal),
        ("1.0", "1.0.1", Less),
        ("2", "1.9.9", Greater),
    ] {
        assert_eq!(compare(a, b), Some(order), "{a} against {b}");
        assert_eq!(compare(b, a), Some(order.reverse()), "{b} against {a}");
    }
}

#[test]
fn a_difference_in_the_numbers_decides_even_when_text_follows() {
    // Homebrew ships versions such as these.
    assert_eq!(compare("3.6.6", "3.6.7-dbc92f97"), Some(Less));
    assert_eq!(compare("1.9.2", "1.9.3b"), Some(Less));
    assert_eq!(compare("1.9.3b", "1.9.2"), Some(Greater));
}

#[test]
fn what_cannot_be_ordered_is_not_guessed() {
    for (a, b) in [
        ("1.9.3", "1.9.3b"),
        ("3.6.7-aaa", "3.6.7-bbb"),
        ("beta", "rc"),
        ("1.0", "1.0.beta"),
        ("1..2", "1.0.2"),
        ("2.0", "2.0,200"),
        ("2.0,200", "2.0"),
    ] {
        assert_eq!(compare(a, b), None, "{a} against {b}");
        assert_eq!(compare(b, a), None, "{b} against {a}");
    }
    // Identical text needs no reading.
    assert_eq!(compare("beta", "beta"), Some(Equal));
}

#[test]
fn a_build_number_after_a_comma_orders_when_both_have_one() {
    assert_eq!(compare("2.0,100", "2.0,200"), Some(Less));
    assert_eq!(compare("2.0,200", "2.0,200"), Some(Equal));
    assert_eq!(
        compare("2.0,300", "2.1,200"),
        Some(Less),
        "the version decides first"
    );
    // Different lists whose shared part already differs are decided by it.
    assert_eq!(compare("3.0", "2.0,200"), Some(Greater));
    assert_eq!(compare("1.0", "2.0,200"), Some(Less));
}

#[test]
fn an_update_is_offered_unless_the_app_is_known_to_be_at_or_past_the_cask() {
    for (current, target, offered) in [
        // The reviewer's downgrade: a cask without auto_updates, an app that
        // updated itself past it.
        ("3.0", "2.0", false),
        ("0.12.1", "0.12.1", false),
        ("1.0", "1.0.0", false),
        ("1.139.1", "1.142.0", true),
        // A build-number-only update is real.
        ("2.0,100", "2.0,200", true),
        ("2.0", "2.0,200", true),
        ("2.0,200", "2.0,200", false),
        // Order unknown: kept, because hiding it would hide a real update.
        ("1.9.3", "1.9.3b", true),
        ("beta", "rc", true),
    ] {
        assert_eq!(
            offers_update(current, target),
            offered,
            "{current} to {target}"
        );
    }
}
