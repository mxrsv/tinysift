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
//! No string lowercasing or normalisation happens here, symlinks are never
//! replaced by their targets (an alias path is a different path), and
//! renames are not tracked. Anything that stops equivalence from being
//! decided (unreadable directory, several entries for one inode, two
//! different missing names, a malformed path) is reported as an
//! [`Uncertainty`], which callers must treat as blocked.
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

/// The two filesystem reads spelling resolution needs.
pub trait DirReader {
    /// Entry names with their inode numbers; `NotFound` when `dir` is absent.
    fn list(&self, dir: &Path) -> io::Result<Vec<(OsString, u64)>>;
    /// Inode of `path` without following a final symlink.
    fn lookup_ino(&self, path: &Path) -> io::Result<u64>;
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
}

fn is_missing(e: &io::Error) -> bool {
    e.kind() == io::ErrorKind::NotFound || e.raw_os_error() == Some(libc::ENOTDIR)
}

type Listing = Result<Option<HashMap<OsString, u64>>, io::ErrorKind>;
type NameResolution = Result<Option<OsString>, Doubt>;

/// Decides what a rule list says about candidate paths. Caches directory
/// reads, so it is only valid for one moment: build a new one per check.
#[derive(Default)]
pub struct Matcher<R = RealFs> {
    reader: R,
    listings: RefCell<HashMap<PathBuf, Rc<Listing>>>,
    names: RefCell<HashMap<(PathBuf, OsString), NameResolution>>,
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
        for rule in rules {
            let related = path_components(&rule.path)
                .map_err(Doubt::InvalidPath)
                .and_then(|names| self.relate(&cand, &names));
            match related {
                Ok(Some(Relation::InsideRule)) if rule.scope == RuleScope::File => {}
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
mod tests {
    use std::collections::HashSet;
    use std::fs;
    use std::os::unix::ffi::OsStrExt;
    use std::os::unix::fs::symlink;

    use super::Relation::{CarriesRule, Equal, InsideRule};
    use super::*;

    fn rule(path: &Path, scope: RuleScope) -> ProtectionRule {
        let id = "r1".into();
        let path = path.to_path_buf();
        ProtectionRule { id, path, scope }
    }

    fn dir_rule(path: &Path) -> ProtectionRule {
        rule(path, RuleScope::Directory)
    }

    /// The relation of `root/rel` ("" is `root`) to the rules, on the real disk.
    fn rel(rules: &[ProtectionRule], root: &Path, rel: &str) -> Option<Relation> {
        let path = if rel.is_empty() {
            root.into()
        } else {
            root.join(rel)
        };
        let eval = Matcher::new().evaluate(rules, &path);
        assert!(eval.uncertain.is_empty(), "{:?}", eval.uncertain);
        eval.blocks.first().map(|b| b.relation)
    }

    fn mkdirs(root: &Path, names: &[&str]) {
        for name in names {
            fs::create_dir_all(root.join(name)).unwrap();
        }
    }

    /// Whether `root`'s volume treats names that differ only in case as one.
    fn volume_ignores_case(root: &Path) -> bool {
        mkdirs(root, &["CaseProbe"]);
        root.join("caseprobe").exists()
    }

    fn sample() -> RulesDocument {
        RulesDocument {
            version: RULES_VERSION,
            revision: 1,
            next_id: 2,
            rules: vec![dir_rule(Path::new("/a/b"))],
        }
    }

    fn invalid(edit: &dyn Fn(&mut RulesDocument)) -> RuleError {
        let mut doc = sample();
        edit(&mut doc);
        doc.validate().unwrap_err()
    }

    #[test]
    fn rule_paths_must_be_canonical_absolute_and_bounded() {
        assert_eq!(sample().validate(), Ok(()));
        let long = format!("/{}", "x".repeat(MAX_COMPONENT_BYTES + 1));
        let bad_paths = [
            ("a/b", RuleError::NotAbsolute),
            ("/a/../b", RuleError::NotNormalized),
            ("/a//b", RuleError::NotNormalized),
            ("/a/b/", RuleError::NotNormalized),
            ("/", RuleError::IsRoot),
            ("/a/\u{0}", RuleError::ContainsNul),
            (long.as_str(), RuleError::TooLong),
        ];
        for (path, want) in bad_paths {
            assert_eq!(
                invalid(&|d| d.rules[0].path = path.into()),
                want,
                "{path:?}"
            );
        }
        let non_utf8 = PathBuf::from(OsStr::from_bytes(b"/a/\xff"));
        assert_eq!(validate_rule_path(&non_utf8), Err(RuleError::NotUtf8));
    }

    #[test]
    fn documents_validate_ids_counters_duplicates_and_bounds() {
        for id in ["", "r", "x1", "r01", "r-1", "r2", "internal:store"] {
            let bad = invalid(&|d| d.rules[0].id = id.into());
            assert_eq!(bad, RuleError::BadId(id.into()), "{id}");
        }
        assert_eq!(
            invalid(&|d| d.version = 2),
            RuleError::UnsupportedVersion(2)
        );
        let zero = invalid(&|d| d.revision = 0);
        assert!(matches!(zero, RuleError::BadCounters { .. }));
        let again = dir_rule(Path::new("/c"));
        let same_id = invalid(&|d| d.rules.push(again.clone()));
        assert_eq!(same_id, RuleError::DuplicateId("r1".into()));
        let mut twin = dir_rule(Path::new("/a/b"));
        twin.id = "r2".into();
        let same_path = invalid(&|d| {
            d.next_id = 3;
            d.rules.push(twin.clone());
        });
        assert_eq!(same_path, RuleError::DuplicatePath("/a/b".into()));
        let many = invalid(&|d| d.rules = vec![again.clone(); MAX_RULES + 1]);
        assert_eq!(many, RuleError::TooManyRules);
    }

    #[test]
    fn a_string_prefix_neighbour_is_not_matched() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        mkdirs(root, &["foo/inner", "foobar/inner"]);
        let rules = [dir_rule(&root.join("foo"))];
        assert_eq!(rel(&rules, root, "foobar"), None);
        assert_eq!(rel(&rules, root, "foobar/inner"), None);
        assert_eq!(rel(&rules, root, "foo/inner"), Some(InsideRule));
    }

    #[test]
    fn directory_rules_cover_descendants_and_every_rule_blocks_its_ancestors() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        mkdirs(root, &["a/b/c"]);
        let dir = [dir_rule(&root.join("a/b"))];
        assert_eq!(rel(&dir, root, "a/b"), Some(Equal));
        assert_eq!(rel(&dir, root, "a/b/c"), Some(InsideRule));
        assert_eq!(rel(&dir, root, "a"), Some(CarriesRule));
        assert_eq!(rel(&dir, root, ""), Some(CarriesRule));
        assert_eq!(rel(&dir, Path::new("/"), ""), Some(CarriesRule));
        let file = [rule(&root.join("a/b"), RuleScope::File)];
        assert_eq!(rel(&file, root, "a/b"), Some(Equal));
        assert_eq!(rel(&file, root, "a/b/c"), None, "no descendants");
        assert_eq!(rel(&file, root, "a"), Some(CarriesRule));
    }

    #[test]
    fn missing_and_renamed_paths_are_kept_but_never_followed() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        mkdirs(root, &["other", "old"]);
        let gone = [dir_rule(&root.join("gone/deep"))];
        assert_eq!(rel(&gone, root, "other"), None, "stored");
        mkdirs(root, &["gone/deep/child"]);
        assert_eq!(rel(&gone, root, "gone/deep"), Some(Equal), "recreated");
        assert_eq!(rel(&gone, root, "gone/deep/child"), Some(InsideRule));
        assert_eq!(rel(&gone, root, "gone"), Some(CarriesRule));
        let old = [dir_rule(&root.join("old"))];
        fs::rename(root.join("old"), root.join("new")).unwrap();
        assert_eq!(rel(&old, root, "new"), None, "renames are not followed");
    }

    #[test]
    fn symlinks_are_identity_not_a_way_to_reach_the_target() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        mkdirs(root, &["real/inner"]);
        symlink(root.join("real"), root.join("link2")).unwrap();
        symlink(root.join("link2"), root.join("link1")).unwrap();
        let chain = [dir_rule(&root.join("link1/inner"))];
        assert_eq!(rel(&chain, root, "real/inner"), None);
        assert_eq!(rel(&chain, root, "link2/inner"), None);
        assert_eq!(rel(&chain, root, "link1/inner"), Some(Equal));
        assert_eq!(rel(&chain, root, "link1"), Some(CarriesRule));
        let real = [dir_rule(&root.join("real"))];
        assert_eq!(rel(&real, root, "link1"), None);
    }

    #[test]
    fn real_volume_decides_whether_spelling_variants_are_one_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        mkdirs(root, &["Cache/sub"]);
        let same = volume_ignores_case(root);
        let expect = |relation| same.then_some(relation);
        let rules = [dir_rule(&root.join("Cache"))];
        assert_eq!(rel(&rules, root, "cache"), expect(Equal));
        assert_eq!(rel(&rules, root, "cache/sub"), expect(InsideRule));
        let deeper = [dir_rule(&root.join("cache/sub"))];
        assert_eq!(rel(&deeper, root, "Cache"), expect(CarriesRule));
        let (nfc, nfd) = ("caf\u{e9}", "cafe\u{301}");
        mkdirs(root, &[nfd]);
        let unicode = [dir_rule(&root.join(nfd))];
        let folded = root.join(nfc).exists();
        assert_eq!(rel(&unicode, root, nfc), folded.then_some(Equal));
        let kind = if same { "insensitive" } else { "sensitive" };
        eprintln!("spelling tests ran on a case-{kind} volume; the other kind is a native gate");
    }

    /// In-memory tree with a chosen case rule, for behaviours a real volume
    /// cannot be forced into.
    struct FakeFs {
        fold: bool,
        dirs: HashMap<PathBuf, Vec<(OsString, u64)>>,
        unreadable: HashSet<PathBuf>,
    }

    impl FakeFs {
        fn new(fold: bool, entries: &[(&str, u64)]) -> Self {
            let dirs = HashMap::from([(PathBuf::from("/"), Vec::new())]);
            let mut fs = Self {
                fold,
                dirs,
                unreadable: HashSet::new(),
            };
            for (path, ino) in entries {
                let path = Path::new(path);
                fs.dirs.entry(path.to_path_buf()).or_default();
                let name = path.file_name().unwrap().to_os_string();
                fs.dirs
                    .get_mut(path.parent().unwrap())
                    .unwrap()
                    .push((name, *ino));
            }
            fs
        }

        fn find(&self, dir: &Path, name: &OsStr) -> Option<(OsString, u64)> {
            let fold = |s: &OsStr| s.to_string_lossy().to_lowercase();
            let same = |n: &OsStr| n == name || (self.fold && fold(n) == fold(name));
            self.dirs[dir].iter().find(|(n, _)| same(n)).cloned()
        }

        fn real_dir(&self, path: &Path) -> Option<PathBuf> {
            let mut real = PathBuf::from("/");
            for name in path_components(path).unwrap() {
                real.push(self.find(&real, &name)?.0);
            }
            Some(real)
        }
    }

    impl DirReader for FakeFs {
        fn list(&self, dir: &Path) -> io::Result<Vec<(OsString, u64)>> {
            let real = self.real_dir(dir).ok_or(io::ErrorKind::NotFound)?;
            match self.unreadable.contains(&real) {
                true => Err(io::ErrorKind::PermissionDenied.into()),
                false => Ok(self.dirs[&real].clone()),
            }
        }

        fn lookup_ino(&self, path: &Path) -> io::Result<u64> {
            let parent = self.real_dir(path.parent().unwrap());
            let found = parent.and_then(|dir| self.find(&dir, path.file_name().unwrap()));
            found
                .map(|(_, ino)| ino)
                .ok_or_else(|| io::ErrorKind::NotFound.into())
        }
    }

    const TREE: &[(&str, u64)] = &[
        ("/t", 1),
        ("/t/Cache", 2),
        ("/t/Cache/Sub", 3),
        ("/t/Other", 4),
    ];

    fn fake(fold: bool, tree: &[(&str, u64)], rule: &str, candidate: &str) -> Evaluation {
        let matcher = Matcher::with_reader(FakeFs::new(fold, tree));
        matcher.evaluate(&[dir_rule(Path::new(rule))], Path::new(candidate))
    }

    #[test]
    fn a_case_insensitive_volume_matches_spelling_variants() {
        let relation = |rule, candidate| fake(true, TREE, rule, candidate).blocks[0].relation;
        assert_eq!(relation("/t/Cache", "/T/cache/SUB"), InsideRule);
        assert_eq!(relation("/t/cache/sub", "/t/CACHE"), CarriesRule);
        assert!(fake(true, TREE, "/t/Cache", "/t/other").allows_move());
    }

    #[test]
    fn a_case_sensitive_volume_keeps_spelling_variants_apart() {
        let both = [TREE, &[("/t/cache", 9)]].concat();
        assert!(fake(false, &both, "/t/Cache", "/t/cache").allows_move());
        let eval = fake(false, TREE, "/t/Cache", "/t/Cache/Sub");
        assert_eq!(eval.blocks[0].relation, InsideRule);
    }

    #[test]
    fn unresolvable_equivalence_blocks_instead_of_passing() {
        let two_names = [("/t", 1), ("/t/a", 5), ("/t/b", 5)];
        let eval = fake(true, &two_names, "/t/A", "/t/b");
        assert!(matches!(eval.uncertain[0].doubt, Doubt::Ambiguous { .. }));
        assert!(!eval.allows_move());

        let mut fs = FakeFs::new(true, TREE);
        fs.unreadable.insert(PathBuf::from("/t"));
        let matcher = Matcher::with_reader(fs);
        let rules = [dir_rule(Path::new("/t/Cache"))];
        let eval = matcher.evaluate(&rules, Path::new("/t/Other"));
        assert!(matches!(eval.uncertain[0].doubt, Doubt::Unreadable { .. }));
        assert!(!eval.allows_move());
        let eval = matcher.evaluate(&rules, Path::new("/t/Cache/Sub"));
        assert_eq!(eval.blocks[0].relation, InsideRule, "no read needed");
        assert!(eval.uncertain.is_empty());
    }

    #[test]
    fn two_different_missing_names_and_malformed_paths_are_doubtful() {
        let eval = fake(true, TREE, "/t/Missing1", "/t/Missing2");
        assert!(matches!(eval.uncertain[0].doubt, Doubt::BothMissing { .. }));
        let rules = [dir_rule(Path::new("/a"))];
        let eval = Matcher::new().evaluate(&rules, Path::new("a/../b"));
        assert!(matches!(eval.uncertain[0].doubt, Doubt::InvalidPath(_)));
        assert_eq!(eval.uncertain[0].rule_id, None);
        assert!(Matcher::new()
            .evaluate(&[], Path::new("a/../b"))
            .allows_move());
    }
}
