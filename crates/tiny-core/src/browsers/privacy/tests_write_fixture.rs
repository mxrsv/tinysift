//! Made-up profiles, a fake Trash and a fake process list for the Privacy
//! write and Undo tests. Everything lives in a test's own temporary home:
//! the Trash is `<home>/.Trash`, the Undo store `<home>/store`. Nothing
//! touches the real Trash or `~/Library`.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use rusqlite::{params, Connection};

use super::running::{RunningBundles, RunningCheck};
use super::sha256::digest_path;
use super::tests::{chromium_cookies, chromium_history, now, profile, unix_days_ago};
use super::*;
use crate::clean::finder_trash::{Trash, TrashError};
use crate::clean::process::test_support::MockChecker;
use crate::clean::process::AppProbe;

/// A test's own home with a profile folder, a Trash and an Undo store.
pub(super) struct Home {
    _dir: tempfile::TempDir,
    pub root: PathBuf,
}

impl Home {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        // The Trash check wants an absolute path; `/var` is a link on macOS.
        let root = fs::canonicalize(dir.path()).unwrap();
        fs::create_dir_all(root.join("profile")).unwrap();
        Home { _dir: dir, root }
    }

    pub fn dir(&self) -> PathBuf {
        self.root.join("profile")
    }

    pub fn profile(&self) -> BrowserProfile {
        profile(&self.dir())
    }

    pub fn trash_dir(&self) -> PathBuf {
        self.root.join(".Trash")
    }

    pub fn store(&self) -> UndoStore {
        UndoStore::at(self.root.join("store"))
    }

    pub fn trash(&self) -> FakeTrash {
        FakeTrash {
            dir: self.trash_dir(),
            fail: false,
            watch: Some(self.dir().join("Cookies")),
            seen: Mutex::new(Vec::new()),
        }
    }

    /// Every file of the profile folder, by name, with its bytes.
    pub fn listing(&self) -> BTreeMap<String, Vec<u8>> {
        listing(&self.dir())
    }

    pub fn trashed(&self) -> Vec<String> {
        listing(&self.trash_dir()).into_keys().collect()
    }
}

pub(super) fn listing(dir: &Path) -> BTreeMap<String, Vec<u8>> {
    let Ok(entries) = fs::read_dir(dir) else {
        return BTreeMap::new();
    };
    entries
        .flatten()
        .filter(|e| e.file_type().unwrap().is_file())
        .map(|e| {
            (
                e.file_name().to_string_lossy().into_owned(),
                fs::read(e.path()).unwrap(),
            )
        })
        .collect()
}

/// Moves into its own `.Trash` folder and says where. `watch` is a file
/// whose digest is noted at each move, to tell when the backup moved.
pub(super) struct FakeTrash {
    pub dir: PathBuf,
    pub fail: bool,
    pub watch: Option<PathBuf>,
    pub seen: Mutex<Vec<(PathBuf, Option<String>)>>,
}

impl Trash for FakeTrash {
    fn move_to_trash(&self, path: &Path) -> Result<(), TrashError> {
        self.move_to_trash_reporting(path).map(|_| ())
    }

    fn move_to_trash_reporting(&self, path: &Path) -> Result<Option<PathBuf>, TrashError> {
        if self.fail {
            return Err(TrashError::Failed("the fake Trash refuses".into()));
        }
        let watched = self.watch.as_deref().and_then(|w| digest_path(w).ok());
        fs::create_dir_all(&self.dir).unwrap();
        let name = path.file_name().unwrap().to_string_lossy().into_owned();
        let mut dest = self.dir.join(&name);
        let mut n = 2;
        while dest.symlink_metadata().is_ok() {
            dest = self.dir.join(format!("{name} {n}"));
            n += 1;
        }
        fs::rename(path, &dest).map_err(|e| TrashError::Failed(e.to_string()))?;
        self.seen.lock().unwrap().push((dest.clone(), watched));
        Ok(Some(dest))
    }
}

pub(super) struct Bundles(pub Vec<&'static str>);

impl RunningBundles for Bundles {
    fn bundle_ids(&self) -> Result<Vec<String>, String> {
        Ok(self.0.iter().map(|s| s.to_string()).collect())
    }
}

static NO_BUNDLES: Bundles = Bundles(Vec::new());

/// No process of any browser runs.
pub(super) fn quit() -> RunningCheck<'static> {
    RunningCheck {
        names: &Quiet,
        bundles: &NO_BUNDLES,
    }
}

/// No process name runs.
pub(super) struct Quiet;

impl AppProbe for Quiet {
    fn probe(&self, _name: &str) -> Result<bool, String> {
        Ok(false)
    }
}

pub(super) fn env<'a>(
    processes: &'a dyn BrowserProcesses,
    trash: &'a FakeTrash,
    store: &'a UndoStore,
) -> PrivacyEnv<'a> {
    PrivacyEnv {
        processes,
        trash,
        store,
    }
}

pub(super) fn running_named(name: &'static str) -> MockChecker {
    MockChecker::with_running([name])
}

pub(super) fn count(path: &Path, sql: &str) -> i64 {
    Connection::open(path)
        .unwrap()
        .query_row(sql, [], |r| r.get(0))
        .unwrap()
}

pub(super) const KEEP_URL: &str = "https://keep.example.com/";
pub(super) const OLD_URLS: [&str; 2] = ["https://old.example.org/a", "https://old.example.org/b"];

/// A Chrome profile: `keep.example.com` visited 5 days ago, two pages of
/// `old.example.org` last visited 200 and 300 days ago, each with two
/// visits, a favicon mapping, a top site and a shortcut; cookies of the
/// kept site, the old site and two never-visited trackers; `Visited Links`.
pub(super) fn chrome_profile(home: &Home) {
    let dir = home.dir();
    chromium_history(
        &dir,
        &[(KEEP_URL, 5), (OLD_URLS[0], 200), (OLD_URLS[1], 300)],
    );
    let history = Connection::open(dir.join("History")).unwrap();
    for id in 1..=3 {
        for n in 0..2 {
            history
                .execute(
                    "INSERT INTO visits(url, visit_time) VALUES (?1, ?2)",
                    params![id, 13_000_000_000_000_000_i64 + n],
                )
                .unwrap();
        }
    }
    drop(history);
    chromium_cookies(
        &dir.join("Cookies"),
        &[
            (".keep.example.com", "sid", 100),
            (".old.example.org", "pref", 200),
            ("tracker.example.biz", "uid", 150),
            (".ads.example.net", "ad", 150),
        ],
    );
    let urls = [KEEP_URL, OLD_URLS[0], OLD_URLS[1]];
    leftover_db(
        &dir.join("Favicons"),
        "CREATE TABLE icon_mapping(id INTEGER PRIMARY KEY, page_url LONGVARCHAR NOT NULL, icon_id INTEGER);
         CREATE TABLE favicons(id INTEGER PRIMARY KEY, url LONGVARCHAR NOT NULL);",
        "INSERT INTO icon_mapping(page_url, icon_id) VALUES (?1, 1)",
        &urls,
    );
    leftover_db(
        &dir.join("Top Sites"),
        "CREATE TABLE top_sites(url LONGVARCHAR PRIMARY KEY, url_rank INTEGER, title LONGVARCHAR);",
        "INSERT INTO top_sites(url, url_rank, title) VALUES (?1, 0, 'Site')",
        &urls,
    );
    leftover_db(
        &dir.join("Shortcuts"),
        "CREATE TABLE omni_box_shortcuts(id VARCHAR PRIMARY KEY, text VARCHAR, url VARCHAR,
            last_access_time INTEGER, number_of_hits INTEGER);",
        "INSERT INTO omni_box_shortcuts(id, text, url) VALUES (hex(randomblob(8)), 'typed', ?1)",
        &urls,
    );
    fs::write(dir.join("Visited Links"), b"VLnk made-up hash table").unwrap();
}

pub(super) fn leftover_db(path: &Path, schema: &str, insert: &str, urls: &[&str]) {
    let conn = Connection::open(path).unwrap();
    conn.execute_batch(schema).unwrap();
    for url in urls {
        conn.execute(insert, [url]).unwrap();
    }
}

/// A Firefox profile in WAL mode: `example.com` visited 4 days ago,
/// `old.example.org` 200 days ago, `saved.example.net` (bookmarked) 300
/// days ago, two visits each; cookies of the visited site, the old site in
/// two containers and a tracker. With `leave_wal`, the files are taken
/// while the connections are open, as a browser that quit without a
/// checkpoint leaves them.
pub(super) fn firefox_profile(home: &Home, leave_wal: bool) {
    let build = if leave_wal {
        home.root.join("build")
    } else {
        home.dir()
    };
    fs::create_dir_all(&build).unwrap();
    let open = |name: &str| {
        let conn = Connection::open(build.join(name)).unwrap();
        conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0;")
            .unwrap();
        conn
    };
    let places = open("places.sqlite");
    places
        .execute_batch(
            "CREATE TABLE moz_places(id INTEGER PRIMARY KEY, url LONGVARCHAR, title LONGVARCHAR,
                rev_host LONGVARCHAR, visit_count INTEGER DEFAULT 0, last_visit_date INTEGER,
                guid TEXT, foreign_count INTEGER DEFAULT 0 NOT NULL);
             CREATE TABLE moz_historyvisits(id INTEGER PRIMARY KEY, from_visit INTEGER,
                place_id INTEGER, visit_date INTEGER, visit_type INTEGER, session INTEGER);",
        )
        .unwrap();
    for (id, url, days, foreign) in [
        (1, "https://example.com/", 4, 0),
        (2, "https://old.example.org/", 200, 0),
        (3, "https://saved.example.net/", 300, 1),
    ] {
        places
            .execute(
                "INSERT INTO moz_places(id, url, last_visit_date, foreign_count) VALUES (?1, ?2, ?3, ?4)",
                params![id, url, unix_days_ago(days), foreign],
            )
            .unwrap();
        for _ in 0..2 {
            places
                .execute(
                    "INSERT INTO moz_historyvisits(place_id, visit_date) VALUES (?1, ?2)",
                    params![id, unix_days_ago(days)],
                )
                .unwrap();
        }
    }
    let cookies = open("cookies.sqlite");
    cookies
        .execute_batch(
            "CREATE TABLE moz_cookies(id INTEGER PRIMARY KEY, originAttributes TEXT NOT NULL DEFAULT '',
                name TEXT, value TEXT, host TEXT, path TEXT, expiry INTEGER, lastAccessed INTEGER,
                CONSTRAINT moz_uniqueid UNIQUE (name, host, path, originAttributes));",
        )
        .unwrap();
    for (host, name, origin, days) in [
        (".example.com", "sid", "", 100),
        (".example.org", "pref", "", 200),
        (".example.org", "pref", "^userContextId=2", 200),
        (".ads.example.biz", "uid", "", 150),
    ] {
        cookies
            .execute(
                "INSERT INTO moz_cookies(originAttributes, name, value, host, path, lastAccessed)
                 VALUES (?1, ?2, 'v', ?3, '/', ?4)",
                params![origin, name, host, unix_days_ago(days)],
            )
            .unwrap();
    }
    if leave_wal {
        for (name, bytes) in listing(&build) {
            fs::write(home.dir().join(name), bytes).unwrap();
        }
    }
    drop((places, cookies));
}

pub(super) fn scan_of(home: &Home, kind: BrowserKind) -> ProfilePrivacy {
    scan_profile(kind, &home.profile(), now())
}

pub(super) fn write(
    home: &Home,
    kind: BrowserKind,
    consent: &PrivacyConsent,
    processes: &dyn BrowserProcesses,
    trash: &FakeTrash,
) -> Result<PrivacyWriteReport, PrivacyWriteError> {
    let store = home.store();
    remove_privacy(
        kind,
        &home.profile(),
        consent,
        &env(processes, trash, &store),
        now(),
    )
}

/// Everything the scan lists.
pub(super) fn consent_all(scan: &ProfilePrivacy) -> PrivacyConsent {
    PrivacyConsent {
        cookies: scan.cookies_to_remove.clone(),
        history: scan.history_to_remove.clone(),
    }
}
