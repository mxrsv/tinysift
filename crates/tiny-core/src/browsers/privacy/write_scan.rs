//! The consent against a fresh scan: what the write may take now.
//!
//! A consented cookie key is taken only when the fresh scan still lists it
//! for removal (matched on host, name, path and origin attributes; its
//! class may have moved between tracker and old). A history row is taken
//! only when the scan still lists its id. Anything else is skipped, as
//! [`SkipReason::NotFound`] when the profile no longer holds it and as
//! [`SkipReason::NowKept`] when it holds it as kept.

use std::collections::{BTreeMap, BTreeSet, HashSet};

use super::copy::Snapshot;
use super::stores;
use super::write_types::{
    PrivacyConsent, PrivacyWriteReport, SkipReason, SkippedCookie, SkippedHistory,
};
use super::{CookieKey, PrivacyIssue, ProfilePrivacy, Stores};
use crate::browsers::BrowserFamily;

type Identity = (String, String, String, Option<String>);

fn identity(key: &CookieKey) -> Identity {
    (
        key.host.clone(),
        key.name.clone(),
        key.path.clone(),
        key.origin_attributes.clone(),
    )
}

/// What the write takes, and a report holding what it skips.
pub(super) struct Targets {
    pub cookies: Vec<CookieKey>,
    pub history: Vec<i64>,
    pub report: PrivacyWriteReport,
}

pub(super) fn intersect(
    stores: &Stores,
    scan: &ProfilePrivacy,
    consent: &PrivacyConsent,
) -> Result<Targets, PrivacyIssue> {
    let listed: BTreeMap<Identity, &CookieKey> = scan
        .cookies_to_remove
        .iter()
        .map(|key| (identity(key), key))
        .collect();
    let mut seen = HashSet::new();
    let mut cookies = Vec::new();
    let mut unlisted = Vec::new();
    for key in &consent.cookies {
        let id = identity(key);
        if !seen.insert(id.clone()) {
            continue;
        }
        match listed.get(&id) {
            Some(current) => cookies.push((*current).clone()),
            None => unlisted.push(key.clone()),
        }
    }

    let listed_history: BTreeSet<i64> = scan.history_to_remove.iter().copied().collect();
    let consented: BTreeSet<i64> = consent.history.iter().copied().collect();
    let history: Vec<i64> = consented.intersection(&listed_history).copied().collect();
    let unlisted_history: Vec<i64> = consented.difference(&listed_history).copied().collect();

    let mut report = PrivacyWriteReport::default();
    if !unlisted.is_empty() {
        let held = held_cookies(stores)?;
        report.skipped_cookies = unlisted
            .into_iter()
            .map(|key| SkippedCookie {
                reason: reason(held.contains(&identity(&key))),
                key,
            })
            .collect();
    }
    if !unlisted_history.is_empty() {
        let held = held_history(stores)?;
        report.skipped_history = unlisted_history
            .into_iter()
            .map(|id| SkippedHistory {
                id,
                reason: reason(held.contains(&id)),
            })
            .collect();
    }
    Ok(Targets {
        cookies,
        history,
        report,
    })
}

fn reason(held: bool) -> SkipReason {
    if held {
        SkipReason::NowKept
    } else {
        SkipReason::NotFound
    }
}

/// Every cookie identity the profile holds, read on a copy.
fn held_cookies(stores: &Stores) -> Result<HashSet<Identity>, PrivacyIssue> {
    let Some(path) = &stores.cookies else {
        return Ok(HashSet::new());
    };
    let snapshot = Snapshot::of(path)?;
    let rows = match stores.family {
        BrowserFamily::Gecko => stores::firefox_cookies(snapshot.connection(), path)?,
        _ => stores::chromium_cookies(snapshot.connection(), path)?,
    };
    Ok(rows
        .into_iter()
        .map(|row| {
            let i = row.identity;
            (i.host, i.name, i.path, i.origin_attributes)
        })
        .collect())
}

/// Every history row id the profile holds, read on a copy.
fn held_history(stores: &Stores) -> Result<HashSet<i64>, PrivacyIssue> {
    let Some(path) = &stores.history else {
        return Ok(HashSet::new());
    };
    let snapshot = Snapshot::of(path)?;
    let rows = match stores.family {
        BrowserFamily::Gecko => stores::firefox_history(snapshot.connection(), path)?,
        _ => stores::chromium_history(snapshot.connection(), path)?,
    };
    Ok(rows.into_iter().map(|entry| entry.id).collect())
}
