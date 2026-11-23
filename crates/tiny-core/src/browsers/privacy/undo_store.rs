//! Where a privacy write's Undo is kept: one owner-only JSON file,
//! `~/Library/Application Support/Tiny/browser-privacy/undo.json`, beside
//! Clean's protection store, plus `staging/`, where a backup folder is
//! assembled before it moves to the Trash.
//!
//! Per profile (browser and data folder) the file holds a scan generation
//! and at most one [`UndoRecord`], the latest write's. Every scan through
//! [`super::scan_profile_recorded`] raises the generation first, and a
//! record made under an older generation is stale: Undo lasts until the
//! next scan (B25). A write and an Undo hold the exclusive `flock` on
//! `undo.json.lock` from start to end, and so does a scan's raise.
//!
//! A missing file is a first use. A file that cannot be read or parsed is an
//! error, never a fresh start: a write then refuses before it changes
//! anything, and the record a user may still want is not overwritten.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use serde::{Deserialize, Serialize};

use super::files::Content;
use crate::browsers::{BrowserKind, BrowserProfile};

const HOME_RELATIVE: &str = "Library/Application Support/Tiny/browser-privacy";
const FILE_NAME: &str = "undo.json";
const LOCK_NAME: &str = "undo.json.lock";
const STAGING_DIR: &str = "staging";
const VERSION: u32 = 1;
const MAX_BYTES: u64 = 4 << 20;
const DIR_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;

/// Why the Undo store could not be used.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct UndoStoreError(pub String);

/// The Undo store of one account.
#[derive(Debug, Clone)]
pub struct UndoStore {
    dir: PathBuf,
}

/// One file a write recorded: where it is, its name in the backup folder,
/// and its digest before and after the write.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RecordedFile {
    pub path: PathBuf,
    pub backup_name: String,
    pub original: Content,
    /// Meaningful once the record is `finished`.
    pub written: Content,
}

/// What Undo needs for the latest write on a profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct UndoRecord {
    pub id: String,
    /// The profile's scan generation when the write ran.
    pub generation: u64,
    /// The backup folder, in the Trash.
    pub backup: PathBuf,
    pub files: Vec<RecordedFile>,
    /// False from the moment the backup is in the Trash until the write has
    /// finished and `written` is recorded; Undo refuses an unfinished one.
    pub finished: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct ProfileEntry {
    pub browser: BrowserKind,
    pub data_dir: PathBuf,
    pub generation: u64,
    pub record: Option<UndoRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct Document {
    version: u32,
    profiles: Vec<ProfileEntry>,
}

impl Default for Document {
    fn default() -> Self {
        Document {
            version: VERSION,
            profiles: Vec::new(),
        }
    }
}

impl Document {
    pub fn entry(&self, kind: BrowserKind, profile: &BrowserProfile) -> Option<&ProfileEntry> {
        self.profiles
            .iter()
            .find(|e| e.browser == kind && e.data_dir == profile.data_dir)
    }

    pub fn entry_mut(&mut self, kind: BrowserKind, profile: &BrowserProfile) -> &mut ProfileEntry {
        let index = match self
            .profiles
            .iter()
            .position(|e| e.browser == kind && e.data_dir == profile.data_dir)
        {
            Some(index) => index,
            None => {
                self.profiles.push(ProfileEntry {
                    browser: kind,
                    data_dir: profile.data_dir.clone(),
                    generation: 0,
                    record: None,
                });
                self.profiles.len() - 1
            }
        };
        &mut self.profiles[index]
    }
}

impl UndoStore {
    pub fn at(dir: PathBuf) -> Self {
        UndoStore { dir }
    }

    /// The store of the account whose home is `home`.
    pub fn for_home(home: &Path) -> Self {
        Self::at(home.join(HOME_RELATIVE))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// A scan of `profile` starts: any Undo recorded before it is stale.
    pub fn note_scan(
        &self,
        kind: BrowserKind,
        profile: &BrowserProfile,
    ) -> Result<(), UndoStoreError> {
        let lock = self.lock()?;
        let mut document = lock.load()?;
        let entry = document.entry_mut(kind, profile);
        entry.generation = entry.generation.wrapping_add(1);
        lock.save(&document)
    }

    /// The exclusive lock for a read, edit and write; held until dropped.
    pub(super) fn lock(&self) -> Result<StoreLock<'_>, UndoStoreError> {
        self.ensure_dir()?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(FILE_MODE)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(self.dir.join(LOCK_NAME))
            .map_err(|e| io_error("could not open the Undo lock", &e))?;
        file.lock()
            .map_err(|e| io_error("could not lock the Undo store", &e))?;
        Ok(StoreLock {
            store: self,
            _file: file,
        })
    }

    /// The folder, made owner-only when missing; a symlink is refused.
    fn ensure_dir(&self) -> Result<(), UndoStoreError> {
        DirBuilder::new()
            .recursive(true)
            .mode(DIR_MODE)
            .create(&self.dir)
            .map_err(|e| io_error("could not make the Undo folder", &e))?;
        match fs::symlink_metadata(&self.dir) {
            Ok(meta) if meta.file_type().is_dir() => Ok(()),
            _ => Err(UndoStoreError(format!(
                "{} is not a plain folder",
                self.dir.display()
            ))),
        }
    }
}

/// The store, locked.
pub(super) struct StoreLock<'s> {
    store: &'s UndoStore,
    _file: File,
}

impl StoreLock<'_> {
    /// Where a backup folder named `name` is assembled.
    pub fn staging(&self, name: &str) -> PathBuf {
        self.store.dir.join(STAGING_DIR).join(name)
    }

    pub fn load(&self) -> Result<Document, UndoStoreError> {
        let path = self.store.dir.join(FILE_NAME);
        let file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)
        {
            Ok(file) => file,
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(Document::default()),
            Err(e) => return Err(io_error("could not read the Undo store", &e)),
        };
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| io_error("could not read the Undo store", &e))?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(UndoStoreError("the Undo store is too large".into()));
        }
        let document: Document = serde_json::from_slice(&bytes)
            .map_err(|e| UndoStoreError(format!("the Undo store is malformed: {e}")))?;
        if document.version != VERSION {
            return Err(UndoStoreError(format!(
                "the Undo store has version {}, not {VERSION}",
                document.version
            )));
        }
        Ok(document)
    }

    /// Replaces the file through a synced temporary file and a rename.
    pub fn save(&self, document: &Document) -> Result<(), UndoStoreError> {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = self.store.dir.join(FILE_NAME);
        let temp = self.store.dir.join(format!(
            ".{FILE_NAME}.{}.{}.tmp",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let bytes = serde_json::to_vec_pretty(document)
            .map_err(|e| UndoStoreError(format!("could not encode the Undo store: {e}")))?;
        let written = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(FILE_MODE)
            .custom_flags(libc::O_CLOEXEC)
            .open(&temp)
            .and_then(|mut file| {
                file.write_all(&bytes)?;
                file.sync_all()
            })
            .and_then(|()| fs::rename(&temp, &path));
        if written.is_err() {
            // Made above with `create_new`: this call's own file.
            let _ = fs::remove_file(&temp);
        }
        written.map_err(|e| io_error("could not save the Undo store", &e))
    }
}

fn io_error(what: &str, error: &io::Error) -> UndoStoreError {
    UndoStoreError(format!("{what}: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::browsers::privacy::tests::profile;

    #[test]
    fn a_scan_raises_the_generation_of_its_profile_only() {
        let home = tempfile::tempdir().unwrap();
        let store = UndoStore::at(home.path().join("store"));
        let one = profile(&home.path().join("one"));
        let two = profile(&home.path().join("two"));
        store.note_scan(BrowserKind::Chrome, &one).unwrap();
        store.note_scan(BrowserKind::Chrome, &one).unwrap();
        store.note_scan(BrowserKind::Brave, &two).unwrap();
        let document = store.lock().unwrap().load().unwrap();
        let generation = |kind, p: &BrowserProfile| document.entry(kind, p).unwrap().generation;
        assert_eq!(generation(BrowserKind::Chrome, &one), 2);
        assert_eq!(generation(BrowserKind::Brave, &two), 1);
        assert!(document.entry(BrowserKind::Chrome, &two).is_none());
    }

    #[test]
    fn a_malformed_store_is_an_error_not_a_fresh_start() {
        let home = tempfile::tempdir().unwrap();
        let store = UndoStore::at(home.path().join("store"));
        fs::create_dir_all(store.dir()).unwrap();
        fs::write(store.dir().join(FILE_NAME), b"{not json").unwrap();
        let p = profile(home.path());
        assert!(store.note_scan(BrowserKind::Chrome, &p).is_err());
        assert_eq!(fs::read(store.dir().join(FILE_NAME)).unwrap(), b"{not json");
    }
}
