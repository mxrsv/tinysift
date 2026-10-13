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
//! store, marker and lock together looks like first use to a session that never
//! saw the store; sessions that did see it report the loss.

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
        self.snapshot = self.store.read_snapshot()?;
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
mod tests {
    use std::os::unix::fs::{symlink, PermissionsExt};

    use super::super::protection::{Matcher, Relation};
    use super::*;

    macro_rules! assert_err {
        ($result:expr, $pattern:pat $(if $guard:expr)?) => {
            match $result {
                Err($pattern) $(if $guard)? => {}
                Err(other) => panic!("unexpected error: {other}"),
                Ok(_) => panic!("expected an error"),
            }
        };
    }

    fn setup() -> (tempfile::TempDir, ProtectionStore, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let data = tmp.path().join("data");
        fs::create_dir_all(data.join("keep/inner")).unwrap();
        fs::create_dir_all(data.join("other")).unwrap();
        let store = ProtectionStore::at(tmp.path().join("Tiny").join(STORE_DIR));
        (tmp, store, data)
    }

    fn save(store: &ProtectionStore, path: &Path) -> Result<ProtectionRule, StoreError> {
        let mut lease = store.lease()?;
        let revision = lease.snapshot().revision;
        lease.add(revision, path, RuleScope::Directory)
    }

    fn relation(snapshot: &PolicySnapshot, candidate: &Path) -> Option<Relation> {
        let eval = snapshot.evaluate(&Matcher::new(), candidate);
        assert!(eval.uncertain.is_empty(), "{:?}", eval.uncertain);
        eval.blocks.first().map(|b| b.relation)
    }

    fn reopen(store: &ProtectionStore) -> ProtectionStore {
        ProtectionStore::at(store.dir().to_path_buf())
    }

    fn rules_file(store: &ProtectionStore) -> PathBuf {
        store.dir().join(RULES_FILE)
    }

    #[test]
    fn rules_survive_reload_and_removal_returns_the_path_to_cleanup() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<ProtectionStore>();
        assert_send_sync::<PolicySnapshot>();
        let (_tmp, store, data) = setup();
        let keep = data.join("keep");
        let absent = store.load().unwrap();
        assert!(!absent.stored && absent.revision == 0 && absent.rules.is_empty());
        assert!(!store.dir().exists() && !store.lock_path().exists());

        assert_eq!(save(&store, &keep).unwrap().id, "r1");
        let reloaded = reopen(&store).load().unwrap();
        assert_eq!((reloaded.revision, reloaded.rules.len()), (1, 1));
        let inner = keep.join("inner");
        assert_eq!(relation(&reloaded, &inner), Some(Relation::InsideRule));
        assert_eq!(relation(&reloaded, &data.join("other")), None);
        let mode = |path: &Path| fs::metadata(path).unwrap().mode() & 0o777;
        assert_eq!(
            (mode(store.dir()), mode(&rules_file(&store))),
            (0o700, 0o600)
        );

        assert_eq!(save(&store, &data.join("other")).unwrap().id, "r2");
        let mut lease = store.lease().unwrap();
        lease.remove(2, "r1").unwrap();
        lease.remove(3, "r2").unwrap();
        drop(lease);
        let emptied = store.load().unwrap();
        assert!(emptied.stored && emptied.rules.is_empty() && emptied.revision == 4);
        assert!(store.dir().join(MARKER_FILE).exists(), "the marker stays");
        assert_eq!(save(&store, &keep).unwrap().id, "r3", "ids are not reused");
        let tiny = Path::new("/Users/me/Library/Application Support/Tiny");
        let home = ProtectionStore::for_home(Path::new("/Users/me"));
        assert_eq!(home.dir(), tiny.join(STORE_DIR));
    }

    #[test]
    fn writes_check_revision_duplicates_and_unknown_ids() {
        let (_tmp, store, data) = setup();
        save(&store, &data.join("keep")).unwrap();
        let mut lease = store.lease().unwrap();
        let other = data.join("other");
        let stale = lease.add(0, &other, RuleScope::File);
        assert_err!(stale, StoreError::Stale { actual: 1, .. });
        let dup = lease.add(1, &data.join("keep"), RuleScope::File);
        assert_err!(dup, StoreError::Duplicate(id) if id == "r1");
        assert_err!(lease.remove(1, "r9"), StoreError::UnknownRule(_));
    }

    #[test]
    fn the_store_and_its_lock_are_protected_from_cleanup() {
        let (_tmp, store, data) = setup();
        save(&store, &data.join("keep")).unwrap();
        let snapshot = store.load().unwrap();
        let (lock, rules) = (store.lock_path(), rules_file(&store));
        for protected in [store.dir(), &rules, &lock, store.dir().parent().unwrap()] {
            let eval = snapshot.evaluate(&Matcher::new(), protected);
            assert!(!eval.allows_move(), "{protected:?}");
            let internal = eval
                .blocks
                .iter()
                .all(|b| b.rule_id.starts_with("internal:"));
            assert!(internal);
        }
        let other = data.join("other");
        assert!(snapshot.evaluate(&Matcher::new(), &other).allows_move());
    }

    #[test]
    fn malformed_unknown_version_and_insecure_storage_fail_closed() {
        let (_tmp, store, data) = setup();
        save(&store, &data.join("keep")).unwrap();
        let rules = rules_file(&store);
        let good = fs::read(&rules).unwrap();
        let load_with = |text: &str| {
            fs::write(&rules, text).unwrap();
            store.load()
        };
        assert_err!(load_with("{ not json"), StoreError::Malformed(_));
        let v2 = r#"{"version":2,"revision":1,"next_id":2,"rules":[]}"#;
        assert_err!(
            load_with(v2),
            StoreError::Invalid(RuleError::UnsupportedVersion(_))
        );
        fs::write(&rules, good).unwrap();
        assert!(store.load().is_ok());

        let chmod = |path: &Path, mode| fs::set_permissions(path, fs::Permissions::from_mode(mode));
        chmod(&rules, 0o644).unwrap();
        assert_err!(store.load(), StoreError::UnsafeStorage { .. });
    }

    #[test]
    fn a_missing_rules_file_or_marker_stays_an_error_after_relaunch() {
        let (_tmp, store, data) = setup();
        save(&store, &data.join("keep")).unwrap();
        fs::remove_file(rules_file(&store)).unwrap();
        let relaunched = reopen(&store);
        assert_err!(relaunched.load(), StoreError::Incomplete(_));
        assert_err!(relaunched.lease(), StoreError::Incomplete(_));
        assert!(!rules_file(&store).exists(), "nothing was recreated");
    }

    #[test]
    fn a_symlinked_store_lock_or_parent_is_rejected() {
        let (tmp, store, data) = setup();
        let elsewhere = tmp.path().join("elsewhere");
        fs::create_dir_all(&elsewhere).unwrap();
        let parent = store.dir().parent().unwrap().to_path_buf();
        fs::create_dir_all(&parent).unwrap();
        symlink(&elsewhere, store.dir()).unwrap();
        assert_err!(store.load(), StoreError::UnsafeStorage { .. });
        fs::remove_file(store.dir()).unwrap();

        symlink(elsewhere.join("lock"), store.lock_path()).unwrap();
        assert!(store.lease().is_err());
        assert!(!elsewhere.join("lock").exists(), "not followed");
        fs::remove_file(store.lock_path()).unwrap();

        fs::remove_dir(&parent).unwrap();
        symlink(&elsewhere, &parent).unwrap();
        assert_err!(save(&store, &data), StoreError::UnsafeStorage { .. });
        assert!(fs::read_dir(&elsewhere).unwrap().next().is_none());
    }

    #[test]
    fn the_lock_serializes_separate_handles_and_unwinds() {
        let (_tmp, store, data) = setup();
        save(&store, &data.join("keep")).unwrap();
        let other = reopen(&store);
        let lease = store.lease().unwrap();
        assert_err!(other.lease(), StoreError::Busy);
        assert_err!(other.load(), StoreError::Busy);
        drop(lease);
        let unwinding = || {
            let _held = store.lease().unwrap();
            panic!("while locked");
        };
        assert!(std::panic::catch_unwind(unwinding).is_err());
        other.lease().expect("unwinding frees the lock");
    }

    #[test]
    fn concurrent_writers_each_land_one_rule() {
        let (_tmp, store, data) = setup();
        let writers = 8;
        thread::scope(|scope| {
            for n in 0..writers {
                let handle = reopen(&store);
                let path = data.join(format!("rule-{n}"));
                scope.spawn(move || loop {
                    match save(&handle, &path) {
                        Err(StoreError::Busy) => thread::yield_now(),
                        other => break drop(other.unwrap()),
                    }
                });
            }
        });
        let snapshot = store.load().unwrap();
        assert_eq!(snapshot.revision, writers);
        assert_eq!(snapshot.rules.len(), writers as usize, "unique ids load");
    }

    #[test]
    fn external_edits_and_store_loss_are_detected_never_treated_as_first_use() {
        let (_tmp, store, data) = setup();
        save(&store, &data.join("keep")).unwrap();
        let before = store.load().unwrap();
        let edit = |from: &str, to: &str| {
            let rules = rules_file(&store);
            let text = fs::read_to_string(&rules).unwrap().replace(from, to);
            fs::write(rules, text).unwrap();
        };
        edit("keep", "other");
        let after = store.load().unwrap();
        assert_eq!(after.revision, before.revision);
        assert_ne!(after, before, "content differs at the same revision");
        let mut lease = store.lease().unwrap();
        edit("other", "keep");
        assert_err!(lease.ensure_unchanged(), StoreError::Changed);
        let other = data.join("other");
        assert_err!(lease.add(1, &other, RuleScope::File), StoreError::Changed);

        lease.reload().unwrap();
        fs::remove_dir_all(store.dir()).unwrap();
        assert_err!(lease.ensure_unchanged(), StoreError::StoreLost);
        assert_err!(lease.add(1, &other, RuleScope::File), StoreError::StoreLost);
        assert!(!store.dir().exists(), "no bootstrap over a vanished store");
        assert!(!lease.reload().unwrap().stored, "first use again");
    }

    #[test]
    fn a_failed_update_keeps_the_previous_rules_at_every_step() {
        let (_tmp, store, data) = setup();
        save(&store, &data.join("keep")).unwrap();
        let other = data.join("other");
        for fault in [Fault::BeforeWrite, Fault::BeforeSync, Fault::BeforeRename] {
            let mut lease = store.lease().unwrap();
            store.fail_at(Some(fault));
            let failed = lease.add(1, &other, RuleScope::File);
            store.fail_at(None);
            assert_err!(failed, StoreError::NotSaved(_));
            assert_eq!(lease.reload().unwrap().rules.len(), 1, "{fault:?}");
            assert!(!store.dir().join(RULES_TEMP).exists(), "temp file left");
        }
        let mut lease = store.lease().unwrap();
        store.fail_at(Some(Fault::AfterRename));
        let unsure = lease.add(1, &other, RuleScope::File);
        store.fail_at(None);
        assert_err!(unsure, StoreError::CommitUncertain(_));
        assert_eq!(lease.snapshot().revision, 1, "uncertain is not success");
        assert_err!(lease.ensure_unchanged(), StoreError::Changed);
        assert_eq!(lease.reload().unwrap().rules.len(), 2, "the rename landed");
    }

    #[test]
    fn first_save_failures_before_and_after_publication_recover_by_relaunch() {
        for fault in [Fault::BeforeWrite, Fault::BeforeSync, Fault::BeforePublish] {
            let (_tmp, store, data) = setup();
            store.fail_at(Some(fault));
            assert_err!(save(&store, &data.join("keep")), StoreError::NotSaved(_));
            assert!(!store.dir().exists(), "{fault:?}: nothing was published");

            let relaunched = reopen(&store);
            let loaded = relaunched.load().unwrap();
            assert!(!loaded.stored && loaded.rules.is_empty(), "no policy");
            assert_eq!(save(&relaunched, &data.join("keep")).unwrap().id, "r1");
            assert_eq!(relaunched.load().unwrap().rules.len(), 1);
        }
        let (_tmp, store, data) = setup();
        let (keep, other) = (data.join("keep"), data.join("other"));
        let mut held = store.lease().unwrap();
        store.fail_at(Some(Fault::AfterPublish));
        let first = held.add(0, &keep, RuleScope::Directory);
        store.fail_at(None);
        assert_err!(first, StoreError::CommitUncertain(_));
        // Retrying on the same lease sees the published store, not first use.
        let again = held.add(0, &keep, RuleScope::Directory);
        assert_err!(again, StoreError::Changed);
        drop(held);
        let relaunched = reopen(&store);
        assert_eq!(save(&relaunched, &other).unwrap().id, "r2");
        let after = relaunched.load().unwrap();
        assert_eq!((after.revision, after.rules[0].id.as_str()), (2, "r1"));
    }

    #[test]
    fn staging_never_resets_a_committed_store_and_foreign_staging_is_kept() {
        let (_tmp, store, data) = setup();
        fs::create_dir_all(store.staging_path()).unwrap();
        fs::write(store.staging_path().join("notes.txt"), b"mine").unwrap();
        let blocked = save(&store, &data.join("keep"));
        assert_err!(blocked, StoreError::UnsafeStorage { .. });
        let notes = store.staging_path().join("notes.txt");
        assert!(notes.exists() && !store.dir().exists());
        fs::remove_file(notes).unwrap();

        save(&store, &data.join("keep")).unwrap();
        let committed = store.load().unwrap();
        // Staging from an older interrupted first save is never copied over,
        // or used to rebuild, the committed store.
        fs::create_dir_all(store.staging_path()).unwrap();
        fs::write(store.staging_path().join(RULES_FILE), b"{}").unwrap();
        assert_eq!(store.load().unwrap(), committed);
        let mut lease = store.lease().unwrap();
        lease.add(1, &data.join("other"), RuleScope::File).unwrap();
        drop(lease);
        let after = store.load().unwrap();
        assert_eq!((after.revision, &after.rules[0]), (2, &committed.rules[0]));
        let leftover = fs::read(store.staging_path().join(RULES_FILE)).unwrap();
        assert_eq!(leftover, b"{}", "an update never touches staging");
    }
}
