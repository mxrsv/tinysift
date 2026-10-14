//! Durable storage, bootstrap and locking for the Clean protection rules
//! ([`protection`](super::protection)).
//!
//! With `<store>` = `~/Library/Application Support/Tiny/clean-protection`:
//! `<store>/rules.json` and `<store>/initialized` (marker) are owner-only;
//! `<store>.lock` is one advisory `flock` file beside it; `<store>.staging`
//! is where the first save is assembled. Reads never create anything: no store
//! is the revision-0 "absent" snapshot. The first save assembles rules plus
//! marker in staging, syncs, and renames it to `<store>` under the exclusive
//! lock; a failure before the rename commits nothing and leftover staging is
//! never policy. Later saves replace `rules.json` through a synced temporary
//! sibling and an atomic rename. A store missing its rules or marker is an
//! error, never a fresh start, and a lease that saw a stored policy never
//! bootstraps over a vanished store.
//!
//! Lock order for callers: session gate, this lock, then any clean-state lock.
//! Contention never blocks: it is [`StoreError::Busy`].
//!
//! Limits: `flock` excludes users of the lock, not a process that ignores it
//! or one that unlinks the lock file
//! (tests exercise separate open file descriptions in one process). Removing
//! the complete store directory looks like first use to a session that never
//! saw it, even if the lock remains (the lock also exists before a first save).
//! Callers must retain whether their session ever saw a committed store;
//! `StoreLease` additionally rejects losing that store during a reload.

use std::fs::{self, DirBuilder, File, OpenOptions};
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt};
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use super::protection::{
    rule_id, validate_rule_path, PolicySnapshot, ProtectionRule, RuleError, RuleScope,
    RulesDocument, INTERNAL_ID_PREFIX, RULES_VERSION,
};
use super::trash_plan::{account_home, trusted_home};

const STORE_DIR: &str = "clean-protection";
const LOCK_FILE: &str = "clean-protection.lock";
const STAGING_DIR: &str = "clean-protection.staging";
const RULES_FILE: &str = "rules.json";
const RULES_TEMP: &str = "rules.json.tmp";
const MARKER_FILE: &str = "initialized";
const MARKER_CONTENT: &[u8] = b"tiny clean-protection v1\n";
const HOME_RELATIVE: &str = "Library/Application Support/Tiny";
const MAX_RULES_BYTES: u64 = 4 << 20;
const NOT_PLAIN_DIR: &str = "it is a symlink or not a directory";
/// A lock a session just released can look held for a moment while a child
/// process spawned at that instant still inherits its descriptor, so contention
/// is retried briefly (without ever blocking) before it is reported as `Busy`.
const LOCK_ATTEMPTS: u32 = 8;
const LOCK_RETRY: Duration = Duration::from_millis(5);
const DIR_MODE: u32 = 0o700;
const FILE_MODE: u32 = 0o600;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("clean protection is in use by another Tiny session")]
    Busy,
    #[error("{0}: {1}")]
    Io(String, io::Error),
    #[error("{} is not safe storage: {why}", path.display())]
    UnsafeStorage { path: PathBuf, why: &'static str },
    /// The store exists but its rules file or marker is missing or wrong.
    #[error("the saved protection at {} is incomplete", .0.display())]
    Incomplete(PathBuf),
    #[error("the saved rules are malformed: {0}")]
    Malformed(String),
    #[error("the saved rules are invalid: {0}")]
    Invalid(#[from] RuleError),
    #[error("the saved rules changed (expected revision {expected}, found {actual})")]
    Stale { expected: u64, actual: u64 },
    #[error("the saved rules changed outside this session")]
    Changed,
    #[error("the saved rules disappeared after they were loaded")]
    StoreLost,
    #[error("{0} already protects this path")]
    Duplicate(String),
    #[error("no saved rule {0}")]
    UnknownRule(String),
    /// Nothing was committed; the previous rules are intact.
    #[error("not saved: {0}")]
    NotSaved(io::Error),
    /// The rename happened or may have; reload before trusting either state.
    #[error("the save may or may not have completed; reload: {0}")]
    CommitUncertain(io::Error),
    #[error("cannot determine the account home: {0}")]
    Home(String),
}

fn io_err(op: &'static str, path: &Path) -> impl FnOnce(io::Error) -> StoreError {
    let what = format!("{op} {}", path.display());
    move |source| StoreError::Io(what, source)
}

fn unsafe_storage(path: &Path, why: &'static str) -> StoreError {
    let path = path.to_path_buf();
    StoreError::UnsafeStorage { path, why }
}

/// Points where tests inject a failure (`checkpoint`); no-ops in production.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    BeforeWrite,
    BeforeSync,
    BeforeRename,
    AfterRename,
    BeforePublish,
    AfterPublish,
}

pub struct ProtectionStore {
    dir: PathBuf,
    #[cfg(test)]
    fault: std::sync::Mutex<Option<Fault>>,
}

/// Exclusive hold on the store for one write or cleanup run; dropping it
/// releases the lock.
pub struct StoreLease<'a> {
    store: &'a ProtectionStore,
    _lock: File,
    snapshot: PolicySnapshot,
}

impl ProtectionStore {
    pub fn at(dir: PathBuf) -> Self {
        Self {
            dir,
            #[cfg(test)]
            fault: Default::default(),
        }
    }

    pub fn for_home(home: &Path) -> Self {
        Self::at(home.join(HOME_RELATIVE).join(STORE_DIR))
    }

    /// The store under the trusted account home; never a caller-supplied path.
    pub fn for_account() -> Result<Self, StoreError> {
        let env_home = std::env::var_os("HOME").map(PathBuf::from);
        let account = account_home();
        let home = trusted_home(env_home.as_deref(), account.as_deref());
        Ok(Self::for_home(&home.map_err(StoreError::Home)?))
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn lock_path(&self) -> PathBuf {
        self.dir.with_file_name(LOCK_FILE)
    }

    fn staging_path(&self) -> PathBuf {
        self.dir.with_file_name(STAGING_DIR)
    }

    /// Rules that keep the store, its lock and its staging directory (and so
    /// every ancestor of them) out of cleanup.
    fn internal_rules(&self) -> Vec<ProtectionRule> {
        let internal = |name: &str, path: PathBuf, scope| ProtectionRule {
            id: format!("{INTERNAL_ID_PREFIX}{name}"),
            path,
            scope,
        };
        vec![
            internal("store", self.dir.clone(), RuleScope::Directory),
            internal("staging", self.staging_path(), RuleScope::Directory),
            internal("lock", self.lock_path(), RuleScope::File),
        ]
    }

    /// Reads the policy under a shared lock; an absent store creates nothing.
    pub fn load(&self) -> Result<PolicySnapshot, StoreError> {
        let unlocked = self.read_snapshot()?;
        if !unlocked.stored {
            return Ok(unlocked);
        }
        let _lock = self.lock(false, libc::LOCK_SH)?;
        self.read_snapshot()
    }

    /// Takes the exclusive lock (creating its parent and file) and reads the policy.
    pub fn lease(&self) -> Result<StoreLease<'_>, StoreError> {
        let lock = self.lock(true, libc::LOCK_EX)?;
        let snapshot = self.read_snapshot()?;
        Ok(StoreLease {
            store: self,
            _lock: lock,
            snapshot,
        })
    }

    fn read_snapshot(&self) -> Result<PolicySnapshot, StoreError> {
        let internal = self.internal_rules();
        let meta = match fs::symlink_metadata(&self.dir) {
            Ok(meta) if meta.is_dir() => meta,
            Ok(_) => return Err(unsafe_storage(&self.dir, NOT_PLAIN_DIR)),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(PolicySnapshot::absent(internal))
            }
            Err(e) => return Err(io_err("inspect", &self.dir)(e)),
        };
        if !owner_only(&meta) {
            return Err(unsafe_storage(&self.dir, "it is not owner-only"));
        }
        let marker = self.dir.join(MARKER_FILE);
        let marked = read_owner_only(&marker, MARKER_CONTENT.len() as u64)?;
        if marked.as_deref() != Some(MARKER_CONTENT) {
            return Err(StoreError::Incomplete(marker));
        }
        let rules = self.dir.join(RULES_FILE);
        let bytes =
            read_owner_only(&rules, MAX_RULES_BYTES)?.ok_or(StoreError::Incomplete(rules))?;
        let doc = parse_document(&bytes)?;
        Ok(PolicySnapshot::from_document(doc, internal))
    }

    /// Opens (creating when asked) the lock file in the store's parent and
    /// takes the lock without blocking.
    fn lock(&self, create: bool, operation: libc::c_int) -> Result<File, StoreError> {
        let path = self.lock_path();
        let parent = path.parent().ok_or(unsafe_storage(&path, "no parent"))?;
        if create && !parent.exists() {
            let mut parents = DirBuilder::new();
            let made = parents.recursive(true).mode(DIR_MODE).create(parent);
            made.map_err(io_err("create", parent))?;
        }
        let meta = fs::symlink_metadata(parent).map_err(io_err("inspect", parent))?;
        if !meta.is_dir() {
            return Err(unsafe_storage(parent, NOT_PLAIN_DIR));
        }
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(FILE_MODE)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&path)
            .map_err(io_err("open lock", &path))?;
        for attempt in 1..=LOCK_ATTEMPTS {
            // SAFETY: `file` owns a valid descriptor for the whole call.
            if unsafe { libc::flock(file.as_raw_fd(), operation | libc::LOCK_NB) } == 0 {
                break;
            }
            let e = io::Error::last_os_error();
            match e.kind() {
                io::ErrorKind::WouldBlock if attempt < LOCK_ATTEMPTS => thread::sleep(LOCK_RETRY),
                io::ErrorKind::WouldBlock => return Err(StoreError::Busy),
                _ => return Err(io_err("lock", &path)(e)),
            }
        }
        Ok(file)
    }

    fn checkpoint(&self, at: Fault) -> io::Result<()> {
        #[cfg(test)]
        if *self.fault.lock().unwrap() == Some(at) {
            return Err(io::Error::other(format!("injected failure at {at:?}")));
        }
        let _ = at;
        Ok(())
    }

    #[cfg(test)]
    fn fail_at(&self, at: Option<Fault>) {
        *self.fault.lock().unwrap() = at;
    }

    fn write_synced(&self, path: &Path, bytes: &[u8]) -> io::Result<()> {
        let _ = fs::remove_file(path); // leftover of an earlier failure
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(FILE_MODE)
            .custom_flags(libc::O_NOFOLLOW)
            .open(path)?;
        self.checkpoint(Fault::BeforeWrite)?;
        file.write_all(bytes)?;
        file.flush()?;
        self.checkpoint(Fault::BeforeSync)?;
        file.sync_all()
    }

    /// Replaces `rules.json` atomically.
    fn commit_update(&self, doc: &RulesDocument) -> Result<(), StoreError> {
        let temp = self.dir.join(RULES_TEMP);
        let staged = serde_json::to_vec_pretty(doc)
            .map_err(io::Error::other)
            .and_then(|bytes| self.write_synced(&temp, &bytes))
            .and_then(|()| self.checkpoint(Fault::BeforeRename))
            .and_then(|()| fs::rename(&temp, self.dir.join(RULES_FILE)));
        if let Err(e) = staged {
            let _ = fs::remove_file(&temp);
            return Err(StoreError::NotSaved(e));
        }
        self.checkpoint(Fault::AfterRename)
            .and_then(|()| sync_dir(&self.dir))
            .map_err(StoreError::CommitUncertain)
    }

    /// First save: assemble, sync and atomically publish the whole store.
    fn commit_bootstrap(&self, doc: &RulesDocument) -> Result<(), StoreError> {
        let staging = self.staging_path();
        clear_staging(&staging)?;
        let published = serde_json::to_vec_pretty(doc)
            .map_err(io::Error::other)
            .and_then(|bytes| {
                DirBuilder::new().mode(DIR_MODE).create(&staging)?;
                self.write_synced(&staging.join(RULES_FILE), &bytes)?;
                self.write_synced(&staging.join(MARKER_FILE), MARKER_CONTENT)?;
                sync_dir(&staging)?;
                self.checkpoint(Fault::BeforePublish)?;
                fs::rename(&staging, &self.dir)
            });
        published.map_err(StoreError::NotSaved)?;
        self.checkpoint(Fault::AfterPublish)
            .and_then(|()| sync_dir(self.dir.parent().unwrap_or(Path::new("/"))))
            .map_err(StoreError::CommitUncertain)
    }
}

impl StoreLease<'_> {
    pub fn snapshot(&self) -> &PolicySnapshot {
        &self.snapshot
    }

    /// Re-reads the policy under the held lock and adopts it.
    pub fn reload(&mut self) -> Result<&PolicySnapshot, StoreError> {
        let next = self.store.read_snapshot()?;
        if self.snapshot.stored && !next.stored {
            return Err(StoreError::StoreLost);
        }
        self.snapshot = next;
        Ok(&self.snapshot)
    }

    /// Fails unless the stored policy is exactly what this lease holds.
    pub fn ensure_unchanged(&self) -> Result<(), StoreError> {
        let now = self.store.read_snapshot()?;
        match (now == self.snapshot, self.snapshot.stored && !now.stored) {
            (true, _) => Ok(()),
            (false, true) => Err(StoreError::StoreLost),
            (false, false) => Err(StoreError::Changed),
        }
    }

    /// Saves a rule for `path` (never selects or moves anything) if the store
    /// is unchanged and still at `expected_revision`.
    pub fn add(
        &mut self,
        expected_revision: u64,
        path: &Path,
        scope: RuleScope,
    ) -> Result<ProtectionRule, StoreError> {
        self.ensure_current(expected_revision)?;
        validate_rule_path(path)?;
        if let Some(existing) = self.snapshot.rules.iter().find(|r| r.path == path) {
            return Err(StoreError::Duplicate(existing.id.clone()));
        }
        let id = rule_id(self.snapshot.next_id);
        let rule = ProtectionRule {
            id,
            path: path.into(),
            scope,
        };
        let rules = [self.snapshot.rules.clone(), vec![rule.clone()]].concat();
        self.commit(rules, self.snapshot.next_id + 1)?;
        Ok(rule)
    }

    /// Removes one saved rule by id; the last one leaves a valid empty store.
    pub fn remove(&mut self, expected_revision: u64, id: &str) -> Result<(), StoreError> {
        self.ensure_current(expected_revision)?;
        if !self.snapshot.rules.iter().any(|r| r.id == id) {
            return Err(StoreError::UnknownRule(id.to_string()));
        }
        let rules = self.snapshot.rules.iter().filter(|r| r.id != id);
        self.commit(rules.cloned().collect(), self.snapshot.next_id)
    }

    fn ensure_current(&self, expected: u64) -> Result<(), StoreError> {
        self.ensure_unchanged()?;
        match self.snapshot.revision {
            actual if actual == expected => Ok(()),
            actual => Err(StoreError::Stale { expected, actual }),
        }
    }

    fn commit(&mut self, rules: Vec<ProtectionRule>, next_id: u64) -> Result<(), StoreError> {
        let doc = RulesDocument {
            version: RULES_VERSION,
            revision: self.snapshot.revision + 1,
            next_id,
            rules,
        };
        doc.validate()?;
        // Only a lease that saw no store may create one, and only while the
        // directory is still absent under the lock (`ensure_unchanged`).
        match self.snapshot.stored {
            true => self.store.commit_update(&doc)?,
            false => self.store.commit_bootstrap(&doc)?,
        }
        self.snapshot = PolicySnapshot::from_document(doc, self.store.internal_rules());
        Ok(())
    }
}

fn parse_document(bytes: &[u8]) -> Result<RulesDocument, StoreError> {
    #[derive(serde::Deserialize)]
    struct Probe {
        version: u32,
    }
    let malformed = |e: serde_json::Error| StoreError::Malformed(e.to_string());
    let probe: Probe = serde_json::from_slice(bytes).map_err(malformed)?;
    if probe.version != RULES_VERSION {
        return Err(RuleError::UnsupportedVersion(probe.version).into());
    }
    let doc: RulesDocument = serde_json::from_slice(bytes).map_err(malformed)?;
    doc.validate()?;
    Ok(doc)
}

fn owner_only(meta: &fs::Metadata) -> bool {
    // SAFETY: geteuid has no preconditions.
    meta.uid() == unsafe { libc::geteuid() } && meta.mode() & 0o077 == 0
}

/// Reads a regular owner-only file of at most `limit` bytes without following
/// a symlink; `None` when it does not exist.
fn read_owner_only(path: &Path, limit: u64) -> Result<Option<Vec<u8>>, StoreError> {
    let opened = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(path);
    let file = match opened {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) if e.raw_os_error() == Some(libc::ELOOP) => {
            return Err(unsafe_storage(path, "it is a symlink"))
        }
        Err(e) => return Err(io_err("open", path)(e)),
    };
    let meta = file.metadata().map_err(io_err("inspect", path))?;
    if !owner_only(&meta) {
        return Err(unsafe_storage(path, "it is not owner-only"));
    }
    if !meta.is_file() || meta.len() > limit {
        return Err(unsafe_storage(path, "it is not a plain file"));
    }
    let mut bytes = Vec::new();
    let read = file.take(limit).read_to_end(&mut bytes);
    read.map_err(io_err("read", path))?;
    Ok(Some(bytes))
}

fn sync_dir(dir: &Path) -> io::Result<()> {
    File::open(dir)?.sync_all()
}

/// Removes an interrupted first save's staging directory. Only the files this
/// module writes may be in it; anything else is left alone and reported.
fn clear_staging(staging: &Path) -> Result<(), StoreError> {
    match fs::symlink_metadata(staging) {
        Ok(meta) if meta.is_dir() => {}
        Ok(_) => return Err(unsafe_storage(staging, NOT_PLAIN_DIR)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(io_err("inspect", staging)(e)),
    }
    let entries = fs::read_dir(staging).map_err(io_err("list", staging))?;
    for entry in entries {
        let entry = entry.map_err(io_err("list", staging))?;
        let ours = [RULES_FILE, RULES_TEMP, MARKER_FILE];
        if !ours.iter().any(|name| entry.file_name() == *name) {
            return Err(unsafe_storage(staging, "staging holds unexpected files"));
        }
        fs::remove_file(entry.path()).map_err(io_err("remove", &entry.path()))?;
    }
    fs::remove_dir(staging).map_err(io_err("remove", staging))
}

#[cfg(test)]
mod tests;
