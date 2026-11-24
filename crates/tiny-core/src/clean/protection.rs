//! Persistent "always keep" rules for desktop Clean: the data model, its
//! validation and the path matcher. Persistence and locking live in
//! [`protection_store`](super::protection_store).
//!
//! # Path comparison
//!
//! A rule names one absolute path and a scope (`File` or `Directory`). A
//! candidate is blocked when, comparing path components (never string
//! prefixes):
//!
//! - it equals the rule path (`Equal`);
//! - the rule is a `Directory` rule and the candidate lies inside it
//!   (`InsideRule`);
//! - the candidate is a strict ancestor of the rule path, so moving it would
//!   carry the protected path (`CarriesRule`), whether or not discovery found
//!   the rule's path. A `File` rule never covers descendants.
//!
//! Components are compared left to right. Byte-equal components are equal
//! under any filesystem. At the first differing component both names are
//! resolved to their real directory-entry spelling inside their (already
//! equivalent) parent: list the parent, and when an entry has exactly the
//! looked-up bytes that is the spelling; otherwise `lstat` the looked-up name
//! (the kernel applies the volume's own case and Unicode rules) and take the
//! single listed entry carrying the same inode. The components are the same
//! entry exactly when the spellings are byte-equal. A name the volume cannot
//! find is "missing"; a missing name never equals an existing one.
//!
//! # Real locations
//!
//! A scan spells a path as its provider found it and a person picks one as
//! the system shows it, so one item can arrive under two spellings that
//! differ by a symlink in the folders above it (`/var/folders/…` and
//! `/private/var/folders/…`). Each path is therefore compared twice: as
//! spelled, and at its real location, which is the folder holding it with
//! every symlink resolved plus its own name. A rule holds a candidate when
//! any spelling of one relates to any spelling of the other. The last name is
//! never followed: a link to a kept item is its own entry, and moving the
//! link moves nothing that is kept.
//!
//! No string lowercasing or normalisation happens here and renames are not
//! tracked. Anything that stops equivalence from being decided (unreadable
//! directory, a candidate whose folder cannot be resolved, several entries
//! for one inode, two different missing names, a malformed path) is reported
//! as an [`Uncertainty`], which callers must treat as blocked.
//!
//! Limits: the cache in one [`Matcher`] describes one moment (build a fresh
//! one before every move); rules guard against Tiny's own cleanup, not against
//! someone who deletes all of Tiny's saved state.

use std::cell::RefCell;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::{DirEntryExt, MetadataExt};
use std::path::{Component, Path, PathBuf};
use std::rc::Rc;

use serde::{Deserialize, Serialize};

/// Version written to and accepted from `rules.json`.
pub const RULES_VERSION: u32 = 1;
/// Upper bounds applied to stored and added rules.
pub const MAX_RULES: usize = 2048;
pub const MAX_PATH_BYTES: usize = 4096;
pub const MAX_COMPONENT_BYTES: usize = 255;

/// Prefix of the ids of rules Tiny derives from its own storage paths.
pub const INTERNAL_ID_PREFIX: &str = "internal:";

/// Why a rule, a stored document or a candidate path is not valid.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuleError {
    #[error("the path is not absolute")]
    NotAbsolute,
    #[error("the path is not in canonical form (no `.`, `..`, `//` or trailing `/`)")]
    NotNormalized,
    #[error("the path is not valid UTF-8")]
    NotUtf8,
    #[error("the path contains a NUL byte")]
    ContainsNul,
    #[error("the path or one of its components is too long")]
    TooLong,
    #[error("the file-system root cannot be a rule")]
    IsRoot,
    #[error("invalid rule id {0:?}")]
    BadId(String),
    #[error("duplicate rule id {0:?}")]
    DuplicateId(String),
    #[error("duplicate rule path {0}")]
    DuplicatePath(String),
    #[error("more than {MAX_RULES} rules")]
    TooManyRules,
    #[error("unsupported rules version {0}")]
    UnsupportedVersion(u32),
    #[error("inconsistent counters (revision {revision}, next id {next_id})")]
    BadCounters { revision: u64, next_id: u64 },
}

/// `File` covers exactly the path; `Directory` also everything inside it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RuleScope {
    File,
    Directory,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProtectionRule {
    /// Stable, never reused: `r` followed by a number.
    pub id: String,
    pub path: PathBuf,
    pub scope: RuleScope,
}

/// The `rules.json` content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RulesDocument {
    pub version: u32,
    /// Increases by one on every committed change.
    pub revision: u64,
    /// The number the next rule id uses; ids are never recycled.
    pub next_id: u64,
    pub rules: Vec<ProtectionRule>,
}

/// Splits a canonical absolute path into its names.
pub fn path_components(path: &Path) -> Result<Vec<OsString>, RuleError> {
    if !path.is_absolute() {
        return Err(RuleError::NotAbsolute);
    }
    if path.as_os_str().len() > MAX_PATH_BYTES {
        return Err(RuleError::TooLong);
    }
    let mut names = Vec::new();
    for component in path.components() {
        match component {
            Component::RootDir => {}
            Component::Normal(name) => {
                if name.len() > MAX_COMPONENT_BYTES {
                    return Err(RuleError::TooLong);
                }
                if name.as_bytes().contains(&0) {
                    return Err(RuleError::ContainsNul);
                }
                names.push(name.to_os_string());
            }
            _ => return Err(RuleError::NotNormalized),
        }
    }
    // `components()` silently drops `//`, `/./` and a trailing `/`.
    if join_components(&names).as_os_str() != path.as_os_str() {
        return Err(RuleError::NotNormalized);
    }
    Ok(names)
}

fn join_components(names: &[OsString]) -> PathBuf {
    let mut path = PathBuf::from("/");
    path.extend(names);
    path
}

/// Checks the path a new or stored rule would protect.
pub fn validate_rule_path(path: &Path) -> Result<(), RuleError> {
    if path.to_str().is_none() {
        return Err(RuleError::NotUtf8);
    }
    if path_components(path)?.is_empty() {
        return Err(RuleError::IsRoot);
    }
    Ok(())
}

fn id_number(id: &str) -> Option<u64> {
    let digits = id.strip_prefix('r')?;
    let canonical = !digits.is_empty()
        && digits.bytes().all(|b| b.is_ascii_digit())
        && (digits == "0" || !digits.starts_with('0'));
    canonical.then(|| digits.parse().ok()).flatten()
}

pub fn rule_id(number: u64) -> String {
    format!("r{number}")
}

impl RulesDocument {
    pub fn validate(&self) -> Result<(), RuleError> {
        if self.version != RULES_VERSION {
            return Err(RuleError::UnsupportedVersion(self.version));
        }
        if self.revision == 0 || self.next_id == 0 {
            return Err(RuleError::BadCounters {
                revision: self.revision,
                next_id: self.next_id,
            });
        }
        if self.rules.len() > MAX_RULES {
            return Err(RuleError::TooManyRules);
        }
        let mut ids = std::collections::HashSet::new();
        let mut paths = std::collections::HashSet::new();
        for rule in &self.rules {
            match id_number(&rule.id) {
                Some(n) if n < self.next_id => {}
                _ => return Err(RuleError::BadId(rule.id.clone())),
            }
            if !ids.insert(rule.id.as_str()) {
                return Err(RuleError::DuplicateId(rule.id.clone()));
            }
            validate_rule_path(&rule.path)?;
            if !paths.insert(rule.path.as_path()) {
                return Err(RuleError::DuplicatePath(rule.path.display().to_string()));
            }
        }
        Ok(())
    }
}

/// A validated view of the policy at one moment: what the store held, plus
/// the rules Tiny derives from its own storage. Equality compares revision
/// and content, so an external edit that keeps the revision is detected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicySnapshot {
    /// False while no store exists yet (first use): revision 0, no rules.
    pub stored: bool,
    pub revision: u64,
    pub next_id: u64,
    pub rules: Vec<ProtectionRule>,
    internal: Vec<ProtectionRule>,
}

impl PolicySnapshot {
    pub fn absent(internal: Vec<ProtectionRule>) -> Self {
        Self {
            stored: false,
            revision: 0,
            next_id: 1,
            rules: Vec::new(),
            internal,
        }
    }

    pub fn from_document(doc: RulesDocument, internal: Vec<ProtectionRule>) -> Self {
        Self {
            stored: true,
            revision: doc.revision,
            next_id: doc.next_id,
            rules: doc.rules,
            internal,
        }
    }

    /// Saved rules plus the internal ones that keep Tiny's storage out of
    /// cleanup.
    pub fn guarded(&self) -> impl Iterator<Item = &ProtectionRule> {
        self.rules.iter().chain(self.internal.iter())
    }

    pub fn evaluate<R: DirReader>(&self, matcher: &Matcher<R>, candidate: &Path) -> Evaluation {
        matcher.evaluate(self.guarded(), candidate)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Relation {
    Equal,
    InsideRule,
    CarriesRule,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Block {
    pub rule_id: String,
    pub relation: Relation,
}

/// Why equivalence with a rule could not be decided.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum Doubt {
    #[error("could not read {}: {kind}", dir.display())]
    Unreadable { dir: PathBuf, kind: io::ErrorKind },
    #[error("{} has several entries for {:?}", dir.display(), name)]
    Ambiguous { dir: PathBuf, name: OsString },
    #[error("two different names are missing under {}", dir.display())]
    BothMissing { dir: PathBuf },
    #[error("invalid path: {0}")]
    InvalidPath(RuleError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Uncertainty {
    /// `None` when the candidate itself is unusable.
    pub rule_id: Option<String>,
    pub doubt: Doubt,
}

/// What the rules say about one candidate path.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Evaluation {
    pub blocks: Vec<Block>,
    pub uncertain: Vec<Uncertainty>,
}

impl Evaluation {
    /// True only when no rule blocks the candidate and nothing is in doubt.
    pub fn allows_move(&self) -> bool {
        self.blocks.is_empty() && self.uncertain.is_empty()
    }
}

/// The filesystem reads spelling resolution needs.
pub trait DirReader {
    /// Entry names with their inode numbers; `NotFound` when `dir` is absent.
    fn list(&self, dir: &Path) -> io::Result<Vec<(OsString, u64)>>;
    /// Inode of `path` without following a final symlink.
    fn lookup_ino(&self, path: &Path) -> io::Result<u64>;
    /// `dir` with every symlink in it resolved; `NotFound` when it is absent.
    fn resolve_dir(&self, dir: &Path) -> io::Result<PathBuf>;
}

#[derive(Default)]
pub struct RealFs;

impl DirReader for RealFs {
    fn list(&self, dir: &Path) -> io::Result<Vec<(OsString, u64)>> {
        std::fs::read_dir(dir)?
            .map(|entry| entry.map(|e| (e.file_name(), e.ino())))
            .collect()
    }

    fn lookup_ino(&self, path: &Path) -> io::Result<u64> {
        std::fs::symlink_metadata(path).map(|m| m.ino())
    }

    fn resolve_dir(&self, dir: &Path) -> io::Result<PathBuf> {
        std::fs::canonicalize(dir)
    }
}

fn is_missing(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::NotFound || e.raw_os_error() == Some(libc::ENOTDIR)
}

type Listing = Result<Option<HashMap<OsString, u64>>, io::ErrorKind>;
type NameResolution = Result<Option<OsString>, Doubt>;
/// A folder's real location; `None` when nothing is there.
type RealDir = Result<Option<PathBuf>, io::ErrorKind>;

/// Decides what a rule list says about candidate paths. Caches directory
/// reads, so it is only valid for one moment: build a new one per check.
#[derive(Default)]
pub struct Matcher<R = RealFs> {
    reader: R,
    listings: RefCell<HashMap<PathBuf, Rc<Listing>>>,
    names: RefCell<HashMap<(PathBuf, OsString), NameResolution>>,
    real_dirs: RefCell<HashMap<PathBuf, RealDir>>,
}

impl Matcher {
    pub fn new() -> Self {
        Self::default()
    }
}

impl<R: DirReader> Matcher<R> {
    pub fn with_reader(reader: R) -> Self {
        Self {
            reader,
            listings: RefCell::default(),
            names: RefCell::default(),
            real_dirs: RefCell::default(),
        }
    }

    pub fn evaluate<'a>(
        &self,
        rules: impl IntoIterator<Item = &'a ProtectionRule>,
        candidate: &Path,
    ) -> Evaluation {
        let mut out = Evaluation::default();
        let rules: Vec<&ProtectionRule> = rules.into_iter().collect();
        let cand = match path_components(candidate) {
            Ok(c) => c,
            Err(e) => {
                if !rules.is_empty() {
                    out.uncertain.push(Uncertainty {
                        rule_id: None,
                        doubt: Doubt::InvalidPath(e),
                    });
                }
                return out;
            }
        };
        if rules.is_empty() {
            return out;
        }
        // The candidate as spelled, then where it really is.
        let mut spellings = vec![cand];
        match self.real_location(candidate) {
            Ok(real) => spellings.extend(real),
            Err(kind) => out.uncertain.push(Uncertainty {
                rule_id: None,
                doubt: Doubt::Unreadable {
                    dir: candidate.parent().unwrap_or(candidate).to_path_buf(),
                    kind,
                },
            }),
        }
        for rule in rules {
            let related = path_components(&rule.path)
                .map_err(Doubt::InvalidPath)
                .and_then(|names| {
                    let mut rule_spellings = vec![names];
                    // A rule whose folder is gone or unreadable keeps its
                    // spelling: it applies again when the path returns.
                    rule_spellings.extend(self.real_location(&rule.path).ok().flatten());
                    self.relate_any(&spellings, &rule_spellings, rule.scope)
                });
            match related {
                Ok(Some(relation)) => out.blocks.push(Block {
                    rule_id: rule.id.clone(),
                    relation,
                }),
                Ok(None) => {}
                Err(doubt) => out.uncertain.push(Uncertainty {
                    rule_id: Some(rule.id.clone()),
                    doubt,
                }),
            }
        }
        out
    }

    /// The first relation any spelling of the candidate has to any spelling
    /// of the rule. A `File` rule never covers what lies inside its path. A
    /// doubt counts only when no spelling relates: one sure relation blocks.
    fn relate_any(
        &self,
        candidate: &[Vec<OsString>],
        rule: &[Vec<OsString>],
        scope: RuleScope,
    ) -> Result<Option<Relation>, Doubt> {
        let mut doubt = None;
        for cand in candidate {
            for names in rule {
                match self.relate(cand, names) {
                    Ok(Some(Relation::InsideRule)) if scope == RuleScope::File => {}
                    Ok(Some(relation)) => return Ok(Some(relation)),
                    Ok(None) => {}
                    Err(found) => doubt = doubt.or(Some(found)),
                }
            }
        }
        doubt.map_or(Ok(None), Err)
    }

    /// Where `path` really is: the folder holding it with every symlink
    /// resolved, then its own name as spelled (the last name is never
    /// followed). `None` when that is the spelling given or nothing is at the
    /// folder; an error when the folder is there and cannot be resolved.
    fn real_location(&self, path: &Path) -> Result<Option<Vec<OsString>>, io::ErrorKind> {
        let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
            return Ok(None);
        };
        let real = match self.real_dir(parent)? {
            Some(dir) if dir != parent => dir.join(name),
            _ => return Ok(None),
        };
        Ok(path_components(&real).ok())
    }

    fn real_dir(&self, dir: &Path) -> RealDir {
        if let Some(hit) = self.real_dirs.borrow().get(dir) {
            return hit.clone();
        }
        let resolved = match self.reader.resolve_dir(dir) {
            Ok(real) => Ok(Some(real)),
            Err(e) if is_missing(&e) => Ok(None),
            Err(e) => Err(e.kind()),
        };
        self.real_dirs
            .borrow_mut()
            .insert(dir.to_path_buf(), resolved.clone());
        resolved
    }

    fn relate(&self, cand: &[OsString], rule: &[OsString]) -> Result<Option<Relation>, Doubt> {
        for i in 0.. {
            match (cand.get(i), rule.get(i)) {
                (None, None) => return Ok(Some(Relation::Equal)),
                (Some(_), None) => return Ok(Some(Relation::InsideRule)),
                (None, Some(_)) => return Ok(Some(Relation::CarriesRule)),
                (Some(c), Some(r)) if c == r => {}
                (Some(c), Some(r)) => {
                    if !self.same_entry(&cand[..i], c, &rule[..i], r)? {
                        return Ok(None);
                    }
                }
            }
        }
        unreachable!("the loop returns when either path ends")
    }

    fn same_entry(
        &self,
        cand_dir: &[OsString],
        cand: &OsStr,
        rule_dir: &[OsString],
        rule: &OsStr,
    ) -> Result<bool, Doubt> {
        let cand_dir = join_components(cand_dir);
        let rule_dir = join_components(rule_dir);
        match (
            self.real_name(&cand_dir, cand)?,
            self.real_name(&rule_dir, rule)?,
        ) {
            (Some(a), Some(b)) => Ok(a == b),
            (None, None) => Err(Doubt::BothMissing { dir: cand_dir }),
            _ => Ok(false),
        }
    }

    fn real_name(&self, dir: &Path, name: &OsStr) -> NameResolution {
        let key = (dir.to_path_buf(), name.to_os_string());
        if let Some(hit) = self.names.borrow().get(&key) {
            return hit.clone();
        }
        let resolved = self.resolve_name(dir, name);
        self.names.borrow_mut().insert(key, resolved.clone());
        resolved
    }

    fn resolve_name(&self, dir: &Path, name: &OsStr) -> NameResolution {
        let unreadable = |kind| Doubt::Unreadable {
            dir: dir.to_path_buf(),
            kind,
        };
        let listing = self.listing(dir);
        let entries = match &*listing {
            Ok(Some(entries)) => entries,
            Ok(None) => return Ok(None),
            Err(kind) => return Err(unreadable(*kind)),
        };
        if entries.contains_key(name) {
            return Ok(Some(name.to_os_string()));
        }
        let ino = match self.reader.lookup_ino(&dir.join(name)) {
            Ok(ino) => ino,
            Err(e) if is_missing(&e) => return Ok(None),
            Err(e) => return Err(unreadable(e.kind())),
        };
        let mut same = entries.iter().filter(|(_, i)| **i == ino).map(|(n, _)| n);
        match (same.next(), same.next()) {
            (Some(only), None) => Ok(Some(only.clone())),
            _ => Err(Doubt::Ambiguous {
                dir: dir.to_path_buf(),
                name: name.to_os_string(),
            }),
        }
    }

    fn listing(&self, dir: &Path) -> Rc<Listing> {
        if let Some(hit) = self.listings.borrow().get(dir) {
            return Rc::clone(hit);
        }
        let read = match self.reader.list(dir) {
            Ok(entries) => Ok(Some(entries.into_iter().collect())),
            Err(e) if is_missing(&e) => Ok(None),
            Err(e) => Err(e.kind()),
        };
        let read = Rc::new(read);
        self.listings
            .borrow_mut()
            .insert(dir.to_path_buf(), Rc::clone(&read));
        read
    }
}

#[cfg(test)]
mod tests;
