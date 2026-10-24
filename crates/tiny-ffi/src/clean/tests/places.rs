//! `clean_places` lists what a scan with the same options can return. It
//! reads no filesystem and runs no tool.

use tiny_core::clean::providers::known_category_ids;

use super::*;

/// The desktop's scope: safe and review, plus the Trash row, never the
/// other destructive categories.
fn desktop_options() -> FfiCleanOptions {
    FfiCleanOptions {
        categories: Vec::new(),
        include_review: true,
        include_destructive: false,
        idle_days: 30,
        all_build_output: true,
        include_trash: true,
        tool_commands: true,
    }
}

fn place_ids(options: FfiCleanOptions) -> Vec<String> {
    let session = TestSession::new();
    session
        .clean_places(options)
        .into_iter()
        .map(|place| place.id)
        .collect()
}

#[test]
fn the_places_are_the_registry_minus_what_the_desktop_never_scans() {
    let ids = place_ids(desktop_options());
    let expected: Vec<&str> = known_category_ids()
        .iter()
        .copied()
        .filter(|id| {
            let options = CleanOptions::from(desktop_options());
            let providers = all_providers_with(&options, app_runner());
            let provider = providers.iter().find(|p| p.id() == *id).unwrap();
            provider.risk() != RiskLevel::Destructive || provider.desktop_empties_trash()
        })
        .collect();
    assert_eq!(ids, expected);
    assert!(ids.contains(&"trash".to_string()), "the Trash row counts");
    assert!(
        ids.len() < known_category_ids().len(),
        "report-only destructive categories are not offered"
    );
}

#[test]
fn every_place_has_a_section_and_a_label() {
    let session = TestSession::new();
    for place in session.clean_places(desktop_options()) {
        assert!(!place.label.is_empty(), "{} has no label", place.id);
        assert!(
            category_section(&place.id).is_some(),
            "{} would land in no section",
            place.id
        );
    }
}

#[test]
fn the_places_follow_the_options() {
    let without_trash = FfiCleanOptions {
        include_trash: false,
        ..desktop_options()
    };
    assert!(!place_ids(without_trash).contains(&"trash".to_string()));
    let safe_only = FfiCleanOptions {
        include_review: false,
        ..desktop_options()
    };
    assert!(place_ids(safe_only).len() < place_ids(desktop_options()).len());
}
