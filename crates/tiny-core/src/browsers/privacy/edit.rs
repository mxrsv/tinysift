//! The edit of a profile's original databases, in one SQLite transaction.
//!
//! The first database is opened read-write (never created) and the others
//! are attached with `mode=rw`, so a missing file is an error rather than a
//! new empty database. Every schema takes `locking_mode=EXCLUSIVE` and a
//! busy timeout, the transaction is `BEGIN EXCLUSIVE`, and the schema is
//! checked inside it, by column name: a missing required table or column is
//! an unknown schema and the transaction rolls back. A leftover table
//! (`icon_mapping`, `top_sites`, `omni_box_shortcuts`) that is absent is
//! skipped. Chromium keeps rollback journals, so its databases commit
//! atomically together; Firefox's are in WAL mode, where SQLite commits
//! each attached file on its own.
//!
//! A history row's leftovers are deleted by its URL only when no other
//! history row keeps that URL, so a kept page never loses its icon,
//! top site or shortcut.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior};

use super::files::with_suffix;
use super::schema::{inspect, Db, Found, Role};
use super::{CookieKey, PrivacyIssue, PrivacyIssueKind};

const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// What the edit did.
#[derive(Debug, Default)]
pub(super) struct Removed {
    pub cookie_rows: u64,
    /// Keys that matched no row.
    pub cookies_gone: Vec<CookieKey>,
    pub history_rows: u64,
    pub history_gone: Vec<i64>,
    /// Firefox rows a bookmark or keyword points at now.
    pub history_bookmarked: Vec<i64>,
    pub leftover_rows: u64,
}

/// Deletes the listed cookies and history rows, with the leftovers.
pub(super) fn apply(
    dbs: &[Db],
    cookies: &[CookieKey],
    history: &[i64],
) -> Result<Removed, PrivacyIssue> {
    let Some(first) = dbs.first() else {
        return Ok(Removed::default());
    };
    let fail = |path: &Path| {
        let path = path.to_path_buf();
        move |e: rusqlite::Error| edit_issue(&path, &e)
    };
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let mut conn = Connection::open_with_flags(&first.path, flags).map_err(fail(&first.path))?;
    conn.busy_timeout(BUSY_TIMEOUT).map_err(fail(&first.path))?;
    conn.pragma_update(None, "locking_mode", "EXCLUSIVE")
        .map_err(fail(&first.path))?;
    let mut schemas = Vec::with_capacity(dbs.len());
    for (index, db) in dbs.iter().enumerate() {
        let schema = if index == 0 {
            "main".to_string()
        } else {
            let alias = format!("db{index}");
            conn.execute(&format!("ATTACH DATABASE ?1 AS {alias}"), [uri(&db.path)])
                .map_err(fail(&db.path))?;
            alias
        };
        prepare_schema(&conn, &schema, &db.path).map_err(fail(&db.path))?;
        schemas.push(schema);
    }

    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Exclusive)
        .map_err(fail(&first.path))?;
    let mut found = Vec::with_capacity(dbs.len());
    for (db, schema) in dbs.iter().zip(&schemas) {
        found.push(inspect(&tx, schema, db)?);
    }
    let mut removed = Removed::default();
    delete_cookies(&tx, &found, cookies, &mut removed).map_err(fail(&first.path))?;
    delete_history(&tx, &found, history, &mut removed).map_err(fail(&first.path))?;
    tx.commit().map_err(fail(&first.path))?;
    Ok(removed)
}

/// Exclusive locking, and a journal mode that leaves the files as they
/// were found: `TRUNCATE` keeps a `-journal` file that was there, `DELETE`
/// removes the one this edit makes. WAL stays WAL.
fn prepare_schema(conn: &Connection, schema: &str, path: &Path) -> rusqlite::Result<()> {
    conn.pragma_update(Some(schema), "locking_mode", "EXCLUSIVE")?;
    let mode: String = conn.pragma_query_value(Some(schema), "journal_mode", |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        let keep = with_suffix(path, "-journal").symlink_metadata().is_ok();
        let wanted = if keep { "TRUNCATE" } else { "DELETE" };
        conn.pragma_update_and_check(Some(schema), "journal_mode", wanted, |_| Ok(()))?;
    }
    Ok(())
}

fn find(found: &[Found], role: Role) -> Option<&Found> {
    found.iter().find(|f| f.role == role)
}

fn delete_cookies(
    conn: &Connection,
    found: &[Found],
    keys: &[CookieKey],
    removed: &mut Removed,
) -> rusqlite::Result<()> {
    if keys.is_empty() {
        return Ok(());
    }
    if let Some(db) = find(found, Role::ChromiumCookies) {
        let sql = format!(
            "DELETE FROM {}.cookies WHERE host_key = ?1 AND name = ?2 AND path = ?3",
            db.schema
        );
        for key in keys {
            let rows = conn.execute(&sql, (&key.host, &key.name, &key.path))?;
            tally_cookie(removed, key, rows);
        }
    } else if let Some(db) = find(found, Role::FirefoxCookies) {
        let base = format!(
            "DELETE FROM {}.moz_cookies WHERE host = ?1 AND name = ?2 AND path = ?3",
            db.schema
        );
        let with_origin = format!("{base} AND originAttributes IS ?4");
        for key in keys {
            let rows = match (db.origin_attributes, &key.origin_attributes) {
                (true, origin) => {
                    conn.execute(&with_origin, (&key.host, &key.name, &key.path, origin))?
                }
                (false, None) => conn.execute(&base, (&key.host, &key.name, &key.path))?,
                // The key names a container this table cannot hold.
                (false, Some(_)) => 0,
            };
            tally_cookie(removed, key, rows);
        }
    }
    Ok(())
}

fn tally_cookie(removed: &mut Removed, key: &CookieKey, rows: usize) {
    if rows == 0 {
        removed.cookies_gone.push(key.clone());
    }
    removed.cookie_rows += rows as u64;
}

fn delete_history(
    conn: &Connection,
    found: &[Found],
    ids: &[i64],
    removed: &mut Removed,
) -> rusqlite::Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    if let Some(db) = find(found, Role::ChromiumHistory) {
        let s = &db.schema;
        let leftovers: Vec<String> = [
            (Role::Favicons, "icon_mapping", "page_url"),
            (Role::TopSites, "top_sites", "url"),
            (Role::Shortcuts, "omni_box_shortcuts", "url"),
        ]
        .into_iter()
        .filter_map(|(role, table, column)| {
            let db = find(found, role).filter(|f| f.tables.contains(table))?;
            Some(format!(
                "DELETE FROM {}.{table} WHERE {column} = ?1",
                db.schema
            ))
        })
        .collect();
        for &id in ids {
            let url: Option<Option<String>> = conn
                .query_row(
                    &format!("SELECT url FROM {s}.urls WHERE id = ?1"),
                    [id],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(url) = url else {
                removed.history_gone.push(id);
                continue;
            };
            conn.execute(&format!("DELETE FROM {s}.visits WHERE url = ?1"), [id])?;
            removed.history_rows +=
                conn.execute(&format!("DELETE FROM {s}.urls WHERE id = ?1"), [id])? as u64;
            let Some(url) = url else { continue };
            let still_kept: Option<i64> = conn
                .query_row(
                    &format!("SELECT 1 FROM {s}.urls WHERE url = ?1 LIMIT 1"),
                    [&url],
                    |r| r.get(0),
                )
                .optional()?;
            if still_kept.is_none() {
                for sql in &leftovers {
                    removed.leftover_rows += conn.execute(sql, [&url])? as u64;
                }
            }
        }
    } else if let Some(db) = find(found, Role::Places) {
        let s = &db.schema;
        for &id in ids {
            let foreign: Option<Option<i64>> = conn
                .query_row(
                    &format!("SELECT foreign_count FROM {s}.moz_places WHERE id = ?1"),
                    [id],
                    |r| r.get(0),
                )
                .optional()?;
            match foreign {
                None => removed.history_gone.push(id),
                Some(Some(n)) if n > 0 => removed.history_bookmarked.push(id),
                Some(_) => {
                    conn.execute(
                        &format!("DELETE FROM {s}.moz_historyvisits WHERE place_id = ?1"),
                        [id],
                    )?;
                    removed.history_rows += conn.execute(
                        &format!("DELETE FROM {s}.moz_places WHERE id = ?1 AND IFNULL(foreign_count, 0) = 0"),
                        [id],
                    )? as u64;
                }
            }
        }
    }
    Ok(())
}

/// A `file:` URI for `path` that opens it read-write and never creates it.
fn uri(path: &Path) -> String {
    let mut out = String::from("file:");
    for byte in path.as_os_str().as_encoded_bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                out.push(*byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out.push_str("?mode=rw");
    out
}

pub(super) fn edit_issue(path: &Path, error: &rusqlite::Error) -> PrivacyIssue {
    PrivacyIssue::new(
        PrivacyIssueKind::Unreadable,
        path,
        format!("could not edit the database: {error}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uris_escape_spaces_and_query_characters() {
        assert_eq!(
            uri(Path::new("/a b/Top Sites?#%")),
            "file:/a%20b/Top%20Sites%3F%23%25?mode=rw"
        );
    }

    #[test]
    fn attaching_a_missing_file_fails_instead_of_creating_it() {
        let dir = tempfile::tempdir().unwrap();
        let main = dir.path().join("History");
        Connection::open(&main)
            .unwrap()
            .execute_batch("CREATE TABLE urls(id INTEGER PRIMARY KEY, url TEXT); CREATE TABLE visits(id INTEGER PRIMARY KEY, url INTEGER);")
            .unwrap();
        let dbs = [
            Db {
                role: Role::ChromiumHistory,
                path: main,
            },
            Db {
                role: Role::TopSites,
                path: dir.path().join("Top Sites"),
            },
        ];
        assert!(apply(&dbs, &[], &[1]).is_err());
        assert!(!dir.path().join("Top Sites").exists());
    }
}
