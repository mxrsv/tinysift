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
