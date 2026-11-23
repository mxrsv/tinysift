//! The tables and columns the Privacy edit needs, found by name. A missing
//! required table or column is an unknown schema; an absent leftover table
//! (`icon_mapping`, `top_sites`, `omni_box_shortcuts`) is nothing to do.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use super::copy::Snapshot;
use super::edit::edit_issue;
use super::{PrivacyIssue, PrivacyIssueKind};

/// What a database is, which decides its tables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Role {
    ChromiumCookies,
    ChromiumHistory,
    Favicons,
    TopSites,
    Shortcuts,
    FirefoxCookies,
    Places,
}

const REQUIRED: bool = true;
const LEFTOVER: bool = false;

/// A table, the columns the edit keys on, and whether it must exist.
type Need = (&'static str, &'static [&'static str], bool);

impl Role {
    fn needs(self) -> &'static [Need] {
        match self {
            Role::ChromiumCookies => &[("cookies", &["host_key", "name", "path"], REQUIRED)],
            Role::ChromiumHistory => &[
                ("urls", &["id", "url"], REQUIRED),
                ("visits", &["url"], REQUIRED),
            ],
            Role::Favicons => &[("icon_mapping", &["page_url"], LEFTOVER)],
            Role::TopSites => &[("top_sites", &["url"], LEFTOVER)],
            Role::Shortcuts => &[("omni_box_shortcuts", &["url"], LEFTOVER)],
            Role::FirefoxCookies => &[("moz_cookies", &["host", "name", "path"], REQUIRED)],
            Role::Places => &[
                ("moz_places", &["id", "foreign_count"], REQUIRED),
                ("moz_historyvisits", &["place_id"], REQUIRED),
            ],
        }
    }
}

/// A database the edit opens.
#[derive(Debug, Clone)]
pub(super) struct Db {
    pub role: Role,
    pub path: PathBuf,
}

/// A database's tables as found: the tables present among its needs, and
/// whether `moz_cookies` has `originAttributes`.
pub(super) struct Found {
    pub schema: String,
    pub role: Role,
    pub tables: HashSet<&'static str>,
    pub origin_attributes: bool,
}

/// Checks each database's schema on a private copy, before anything is
/// backed up or written.
pub(super) fn check_copies(dbs: &[Db]) -> Result<(), PrivacyIssue> {
    for db in dbs {
        let snapshot = Snapshot::of(&db.path)?;
        inspect(snapshot.connection(), "main", db)?;
    }
    Ok(())
}

/// The needed tables of `db`, as `schema` holds them.
pub(super) fn inspect(conn: &Connection, schema: &str, db: &Db) -> Result<Found, PrivacyIssue> {
    let mut found = Found {
        schema: schema.to_string(),
        role: db.role,
        tables: HashSet::new(),
        origin_attributes: false,
    };
    for &(table, needed, required) in db.role.needs() {
        let columns = columns(conn, schema, table).map_err(|e| edit_issue(&db.path, &e))?;
        if columns.is_empty() {
            if required {
                return Err(unknown(&db.path, format!("no table `{table}`")));
            }
            continue;
        }
        if let Some(missing) = needed.iter().find(|c| !columns.contains(**c)) {
            return Err(unknown(
                &db.path,
                format!("table `{table}` has no column `{missing}`"),
            ));
        }
        if table == "moz_cookies" {
            found.origin_attributes = columns.contains("originattributes");
        }
        found.tables.insert(table);
    }
    Ok(found)
}

/// The table's column names, lower-cased; empty when there is no such table.
fn columns(conn: &Connection, schema: &str, table: &str) -> rusqlite::Result<HashSet<String>> {
    let mut statement = conn.prepare("SELECT name FROM pragma_table_info(?1, ?2)")?;
    let names = statement.query_map([table, schema], |row| row.get::<_, String>(0))?;
    names.map(|n| n.map(|n| n.to_ascii_lowercase())).collect()
}

fn unknown(path: &Path, detail: String) -> PrivacyIssue {
    PrivacyIssue::new(PrivacyIssueKind::UnknownSchema, path, detail)
}
