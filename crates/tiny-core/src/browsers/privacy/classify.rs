//! The cookie and history rules, on rows already read. Pure: every time is
//! microseconds since the Unix epoch and `cutoff` is the oldest moment that
//! still counts as recent.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use super::site::site_of_host;
use super::stores::{CookieIdentity, CookieRow, HistoryEntry};
use super::{CookieKey, RemovedCookie};

/// What [`tally`] found, before it is tied to a profile.
#[derive(Debug, Default)]
pub(super) struct Tally {
    pub trackers: u64,
    pub old: u64,
    pub kept: u64,
    pub tracker_sites: u64,
    pub old_sites: u64,
    pub history_is_empty: bool,
    pub cookies_to_remove: Vec<CookieKey>,
    pub history_to_remove: Vec<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Class {
    Kept,
    Remove(RemovedCookie),
}

/// The sites the history knows.
struct HistoryIndex {
    /// Sites with any history row.
    seen: HashSet<String>,
    /// Sites with a visit after the cutoff.
    visited: HashSet<String>,
    /// Some row carries a visit time.
    has_visits: bool,
}

impl HistoryIndex {
    fn new(entries: &[HistoryEntry], cutoff: i64) -> Self {
        let mut index = HistoryIndex {
            seen: HashSet::new(),
            visited: HashSet::new(),
            has_visits: false,
        };
        for entry in entries {
            index.has_visits |= entry.last_visit.is_some();
            let Some(site) = &entry.site else { continue };
            if entry.last_visit.is_some_and(|t| t >= cutoff) {
                index.visited.insert(site.clone());
            }
            index.seen.insert(site.clone());
        }
        index
    }
}

fn classify(site: &str, last_access: Option<i64>, history: &HistoryIndex, cutoff: i64) -> Class {
    let used_recently = last_access.is_some_and(|t| t >= cutoff);
    if !history.has_visits {
        // No history to judge by: only a recorded use before the cutoff
        // makes a cookie old, and nothing is a tracker.
        return match last_access {
            Some(_) if !used_recently => Class::Remove(RemovedCookie::Old),
            _ => Class::Kept,
        };
    }
    if used_recently || history.visited.contains(site) {
        Class::Kept
    } else if history.seen.contains(site) {
        Class::Remove(RemovedCookie::Old)
    } else {
        Class::Remove(RemovedCookie::Tracker)
    }
}

/// Classifies every cookie row and picks the old history entries.
///
/// Rows sharing a [`CookieIdentity`] (Chromium keeps one per partition and
/// source port) are removed together, so when any of them is kept all are.
pub(super) fn tally(cookies: &[CookieRow], history: &[HistoryEntry], cutoff: i64) -> Tally {
    let index = HistoryIndex::new(history, cutoff);
    let mut by_identity: BTreeMap<&CookieIdentity, (Class, u64, Option<String>)> = BTreeMap::new();
    for row in cookies {
        let site = site_of_host(&row.identity.host);
        let class = match &site {
            Some(site) => classify(site, row.last_access, &index, cutoff),
            // A row with no host names no site anyone visits; leave it.
            None => Class::Kept,
        };
        let slot = by_identity.entry(&row.identity).or_insert((class, 0, site));
        if class == Class::Kept {
            slot.0 = Class::Kept;
        }
        slot.1 += 1;
    }

    let mut tally = Tally {
        history_is_empty: !index.has_visits,
        ..Tally::default()
    };
    let mut tracker_sites = BTreeSet::new();
    let mut old_sites = BTreeSet::new();
    for (identity, (class, rows, site)) in by_identity {
        let reason = match class {
            Class::Kept => {
                tally.kept += rows;
                continue;
            }
            Class::Remove(reason) => reason,
        };
        let site = site.unwrap_or_default();
        match reason {
            RemovedCookie::Tracker => {
                tally.trackers += rows;
                tracker_sites.insert(site);
            }
            RemovedCookie::Old => {
                tally.old += rows;
                old_sites.insert(site);
            }
        }
        tally.cookies_to_remove.push(identity.key(reason));
    }
    tally.tracker_sites = tracker_sites.len() as u64;
    tally.old_sites = old_sites.len() as u64;

    tally.history_to_remove = history
        .iter()
        .filter(|e| !e.bookmarked && e.last_visit.is_some_and(|t| t < cutoff))
        .map(|e| e.id)
        .collect();
    tally.history_to_remove.sort_unstable();
    tally.history_to_remove.dedup();
    tally
}
