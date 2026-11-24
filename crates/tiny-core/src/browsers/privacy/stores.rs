//! Reading cookie and history rows from a copied database.
//!
//! Columns are found by name through `PRAGMA table_info`, so a browser
//! adding or reordering columns changes nothing. A missing table or
//! required column, or a value of another type, is an unknown schema: the
//! profile is then not scanned rather than guessed at. Times come out as
//! microseconds since the Unix epoch; zero or less means "not recorded".

use std::collections::HashSet;
use std::path::Path;

use rusqlite::types::ValueRef;
use rusqlite::{Connection, Row};

use super::copy::sqlite_issue;
use super::site::site_of_url;
use super::{CookieKey, PrivacyIssue, PrivacyIssueKind, RemovedCookie};

/// Microseconds from 1601-01-01 (Chromium's epoch) to 1970-01-01.
const CHROMIUM_EPOCH_OFFSET_MICROS: i64 = 11_644_473_600 * 1_000_000;

/// The columns that pick out a cookie's rows.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(super) struct CookieIdentity {
    pub host: String,
    pub name: String,
    pub path: String,
    pub origin_attributes: Option<String>,
}

impl CookieIdentity {
    pub fn key(&self, class: RemovedCookie) -> CookieKey {
        CookieKey {
            host: self.host.clone(),
            name: self.name.clone(),
            path: self.path.clone(),
            origin_attributes: self.origin_attributes.clone(),
            class,
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct CookieRow {
    pub identity: CookieIdentity,
    pub last_access: Option<i64>,
}

#[derive(Debug, Clone)]
pub(super) struct HistoryEntry {
    pub id: i64,
    /// `None` for a URL that is not `http`/`https`.
    pub site: Option<String>,
    pub last_visit: Option<i64>,
    /// A Firefox bookmark or keyword points at the row: never removed.
    pub bookmarked: bool,
}

/// One table read: its name and the columns, required then optional.
struct Query<'a> {
    table: &'a str,
    required: &'a [&'a str],
    optional: &'a [&'a str],
}

pub(super) fn chromium_cookies(
    conn: &Connection,
    path: &Path,
) -> Result<Vec<CookieRow>, PrivacyIssue> {
    let query = Query {
        table: "cookies",
        required: &["host_key", "name", "path", "last_access_utc"],
        optional: &[],
    };
    read(conn, path, &query, |row| {
        Ok(CookieRow {
            identity: CookieIdentity {
                host: text(row, 0)?,
                name: text(row, 1)?,
                path: text(row, 2)?,
                origin_attributes: None,
            },
            last_access: int(row, 3)?.and_then(from_chromium_time),
        })
    })
}

pub(super) fn chromium_history(
    conn: &Connection,
    path: &Path,
) -> Result<Vec<HistoryEntry>, PrivacyIssue> {
    let query = Query {
        table: "urls",
        required: &["id", "url", "last_visit_time"],
        optional: &[],
    };
    read(conn, path, &query, |row| {
        Ok(HistoryEntry {
            id: required_int(row, 0)?,
            site: site_of_url(&text(row, 1)?),
            last_visit: int(row, 2)?.and_then(from_chromium_time),
            bookmarked: false,
        })
    })
}

pub(super) fn firefox_cookies(
    conn: &Connection,
    path: &Path,
) -> Result<Vec<CookieRow>, PrivacyIssue> {
    let query = Query {
        table: "moz_cookies",
        required: &["host", "name", "path", "lastAccessed"],
        optional: &["originAttributes"],
    };
    read(conn, path, &query, |row| {
        Ok(CookieRow {
            identity: CookieIdentity {
                host: text(row, 0)?,
                name: text(row, 1)?,
                path: text(row, 2)?,
                origin_attributes: optional_text(row, 4)?,
            },
            last_access: int(row, 3)?.and_then(from_unix_time),
        })
    })
}

pub(super) fn firefox_history(
    conn: &Connection,
    path: &Path,
) -> Result<Vec<HistoryEntry>, PrivacyIssue> {
    let query = Query {
        table: "moz_places",
        required: &["id", "url", "last_visit_date", "foreign_count"],
        optional: &[],
    };
    read(conn, path, &query, |row| {
        Ok(HistoryEntry {
            id: required_int(row, 0)?,
            site: site_of_url(&text(row, 1)?),
            last_visit: int(row, 2)?.and_then(from_unix_time),
            bookmarked: int(row, 3)?.unwrap_or(0) > 0,
        })
    })
}

/// Selects the query's columns from every row. An optional column the
/// table lacks is read as NULL.
fn read<T>(
    conn: &Connection,
    path: &Path,
    query: &Query<'_>,
    mut map: impl FnMut(&Row<'_>) -> Result<T, String>,
) -> Result<Vec<T>, PrivacyIssue> {
    let present = columns(conn, query.table).map_err(|e| sqlite_issue(path, &e))?;
    if present.is_empty() {
        return Err(unknown_schema(path, format!("no table `{}`", query.table)));
    }
    if let Some(missing) = query
        .required
        .iter()
        .find(|c| !present.contains(&c.to_ascii_lowercase()))
    {
        return Err(unknown_schema(
            path,
            format!("table `{}` has no column `{missing}`", query.table),
        ));
    }
    let selected: Vec<String> = query
        .required
        .iter()
        .map(|c| quoted(c))
        .chain(query.optional.iter().map(|c| {
            if present.contains(&c.to_ascii_lowercase()) {
                quoted(c)
            } else {
                "NULL".to_owned()
            }
        }))
        .collect();
    let sql = format!(
        "SELECT {} FROM {}",
        selected.join(", "),
        quoted(query.table)
    );
    let mut statement = conn.prepare(&sql).map_err(|e| sqlite_issue(path, &e))?;
    let mut rows = statement.query([]).map_err(|e| sqlite_issue(path, &e))?;
    let mut out = Vec::new();
    while let Some(row) = rows.next().map_err(|e| sqlite_issue(path, &e))? {
        let value = map(row)
            .map_err(|detail| unknown_schema(path, format!("table `{}`: {detail}", query.table)))?;
        out.push(value);
    }
    Ok(out)
}

/// The table's column names, lower-cased; empty when there is no such
/// table.
fn columns(conn: &Connection, table: &str) -> rusqlite::Result<HashSet<String>> {
    let mut statement = conn.prepare("SELECT name FROM pragma_table_info(?1)")?;
    let names = statement.query_map([table], |row| row.get::<_, String>(0))?;
    names
        .map(|name| name.map(|n| n.to_ascii_lowercase()))
        .collect()
}

/// Names here are this module's constants, never read from a file.
fn quoted(identifier: &str) -> String {
    format!("\"{identifier}\"")
}

fn unknown_schema(path: &Path, detail: String) -> PrivacyIssue {
    PrivacyIssue::new(PrivacyIssueKind::UnknownSchema, path, detail)
}

fn value<'r>(row: &'r Row<'_>, index: usize) -> Result<ValueRef<'r>, String> {
    row.get_ref(index).map_err(|e| e.to_string())
}

/// Text, with NULL read as empty.
fn text(row: &Row<'_>, index: usize) -> Result<String, String> {
    Ok(optional_text(row, index)?.unwrap_or_default())
}

fn optional_text(row: &Row<'_>, index: usize) -> Result<Option<String>, String> {
    match value(row, index)? {
        ValueRef::Null => Ok(None),
        ValueRef::Text(bytes) => Ok(Some(String::from_utf8_lossy(bytes).into_owned())),
        other => Err(format!(
            "column {index} holds {:?}, not text",
            other.data_type()
        )),
    }
}

fn int(row: &Row<'_>, index: usize) -> Result<Option<i64>, String> {
    match value(row, index)? {
        ValueRef::Null => Ok(None),
        ValueRef::Integer(n) => Ok(Some(n)),
        other => Err(format!(
            "column {index} holds {:?}, not an integer",
            other.data_type()
        )),
    }
}

fn required_int(row: &Row<'_>, index: usize) -> Result<i64, String> {
    int(row, index)?.ok_or_else(|| format!("column {index} is NULL"))
}

fn from_chromium_time(micros: i64) -> Option<i64> {
    (micros > 0).then(|| micros - CHROMIUM_EPOCH_OFFSET_MICROS)
}

fn from_unix_time(micros: i64) -> Option<i64> {
    (micros > 0).then_some(micros)
}
