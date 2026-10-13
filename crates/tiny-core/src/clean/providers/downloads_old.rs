use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::SystemTime;

use crate::error::Result;

use super::{execute_per_item, run_tool, CleanProvider};
use crate::clean::fs_safe::{dir_size_checked, is_dir_safe, list_children};
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, Evidence, ExecAction, ExecReport, RiskLevel};
use crate::focus::to_unix;
use crate::runner::{CommandRunner, RealRunner};

const ID: &str = "downloads-old";
const LABEL: &str = "Downloads (old files)";

const MDLS: &str = "/usr/bin/mdls";
/// Fixed `mdls` arguments: print only the last-opened date, raw, with
/// `(null)` for files Spotlight has never seen opened.
const MDLS_ARGS: [&str; 5] = [
    "-name",
    "kMDItemLastUsedDate",
    "-raw",
    "-nullMarker",
    "(null)",
];
const MDLS_NULL: &str = "(null)";
/// Paths per `mdls` spawn; 256 x PATH_MAX stays far below the argv limit.
const MDLS_CHUNK: usize = 256;
const SECS_PER_DAY: i64 = 86_400;

/// Name fragments that mark a file as sensitive, matched against the
/// lowercased name with `-`, `_`, `.` and spaces removed (so "backup code",
/// "backup-code" and "BackupCode" all hit `backupcode`). Add new entries here.
const SENSITIVE_FRAGMENTS: &[&str] = &[
    "recovery",
    "backupcode",
    "secret",
    "password",
    "passwd",
    "passphrase",
    "credential",
    "private",
    "mnemonic",
    "wallet",
    "apikey",
    "apitoken",
    "accesstoken",
];

/// Short words that are too common inside other words (or inside the hex
/// names browsers generate) to match as substrings: `seeds.csv`,
/// `tokenizer.json` and `a2fa91.png` stay unflagged, `seed-phrase.txt` and
/// `github-2fa-codes.txt` are flagged. Matched against the name's
/// alphanumeric words, lowercased.
const SENSITIVE_WORDS: &[&str] = &["2fa", "seed", "token", "tokens"];

/// Extensions (lowercase, no dot) of keys, certificates, vaults and VPN profiles.
/// `key` also hits Keynote files; sensitive files are only kept out of
/// select-all, so that false positive is cheap.
const SENSITIVE_EXTENSIONS: &[&str] = &[
    "pem",
    "key",
    "p12",
    "pfx",
    "cer",
    "crt",
    "der",
    "mobileprovision",
    "keychain",
    "keychain-db",
    "kdbx",
    "1pux",
    "ovpn",
    "gpg",
    "asc",
    "ppk",
];

/// Prefixes of OpenSSH key file names (`id_rsa`, `id_ed25519.pub`, ...).
const SSH_KEY_PREFIXES: &[&str] = &["id_rsa", "id_dsa", "id_ecdsa", "id_ed25519"];

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub struct DownloadsOld {
    pub idle_days: u64,
    runner: Arc<dyn CommandRunner>,
}

impl DownloadsOld {
    pub fn new(idle_days: u64) -> Self {
        Self::with_runner(idle_days, Arc::new(RealRunner))
    }

    pub fn with_runner(idle_days: u64, runner: Arc<dyn CommandRunner>) -> Self {
        Self { idle_days, runner }
    }
}

impl CleanProvider for DownloadsOld {
    fn id(&self) -> &'static str {
        ID
    }
    fn label(&self) -> &'static str {
        LABEL
    }
    fn inclusion_reason(&self) -> String {
        format!(
            "Files directly in Downloads neither modified nor opened for {} days",
            self.idle_days
        )
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    fn desktop_trash_paths(&self) -> bool {
        true
    }
    fn available(&self) -> bool {
        home()
            .map(|h| is_dir_safe(&h.join("Downloads")))
            .unwrap_or(false)
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let h = match home() {
            Some(h) => h,
            None => return Ok(Vec::new()),
        };
        Ok(list_old_files(
            ctx,
            self.runner.as_ref(),
            &h.join("Downloads"),
            self.idle_days,
        ))
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_per_item(items, action, ID)
    }
}

/// Lists every file (not directory) directly inside `dir` that has been
/// neither modified nor opened (Spotlight) for more than `idle_days`.
/// Non-recursive — subdirs are not descended (we don't want to recurse into
/// a user's curated download folders). Hidden files such as `.localized` and
/// `.DS_Store` belong to Finder, not the user. When `mdls` fails, only the
/// mtime decides.
pub fn list_old_files(
    ctx: &ScanContext<'_>,
    runner: &dyn CommandRunner,
    dir: &Path,
    idle_days: u64,
) -> Vec<CleanItem> {
    let candidates = old_by_mtime(ctx, dir, idle_days);
    let opened = last_opened(runner, &candidates);
    let mut out = Vec::new();
    for (path, modified) in candidates {
        let last_opened = opened.get(&path).copied();
        if last_opened.is_some_and(|at| !older_than(at, idle_days)) {
            continue;
        }
        let size = dir_size_checked(&path, ctx);
        out.push(build_item(path, size, modified, last_opened));
    }
    out
}

fn build_item(path: PathBuf, size: u64, modified: i64, last_opened: Option<i64>) -> CleanItem {
    let mut evidence = vec![Evidence::Modified { at: modified }];
    if let Some(at) = last_opened {
        evidence.push(Evidence::LastOpened { at });
    }
    let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
    if let Some(reason) = sensitive_reason(name) {
        evidence.push(Evidence::Sensitive { reason });
    }
    CleanItem {
        category_id: ID.to_string(),
        category_label: LABEL.to_string(),
        path,
        size,
        risk: RiskLevel::Review,
        evidence,
    }
}

/// Regular, non-hidden files older than `idle_days` by mtime, with their
/// mtime (Unix seconds), sorted by path so the `mdls` call is deterministic.
fn old_by_mtime(ctx: &ScanContext<'_>, dir: &Path, idle_days: u64) -> Vec<(PathBuf, i64)> {
    let mut out = Vec::new();
    for path in list_children(dir, ctx) {
        let hidden = path
            .file_name()
            .and_then(|n| n.to_str())
            .is_none_or(|n| n.starts_with('.'));
        if hidden {
            continue;
        }
        let meta = match std::fs::symlink_metadata(&path) {
            Ok(m) => m,
            Err(_) => continue,
        };
        if !meta.file_type().is_file() {
            continue;
        }
        let Ok(mtime) = meta.modified() else { continue };
        let modified = to_unix(mtime) as i64;
        if older_than(modified, idle_days) {
            out.push((path, modified));
        }
    }
    out.sort();
    out
}

fn older_than(at: i64, idle_days: u64) -> bool {
    to_unix(SystemTime::now()) as i64 - at > idle_days as i64 * SECS_PER_DAY
}

/// Spotlight last-opened dates (Unix seconds) for `candidates`, a few
/// batched `mdls` spawns. Files without a date, and everything when `mdls`
/// fails, are simply absent — callers fall back to the mtime.
fn last_opened(runner: &dyn CommandRunner, candidates: &[(PathBuf, i64)]) -> HashMap<PathBuf, i64> {
    let paths: Vec<&Path> = candidates
        .iter()
        .map(|(p, _)| p.as_path())
        .filter(|p| p.to_str().is_some())
        .collect();
    let mut out = HashMap::new();
    for chunk in paths.chunks(MDLS_CHUNK) {
        let Some(dates) = mdls_chunk(runner, chunk) else {
            break;
        };
        for (path, date) in chunk.iter().zip(dates) {
            if let Some(at) = date {
                out.insert(path.to_path_buf(), at);
            }
        }
    }
    out
}

/// One `mdls` call. `-raw` prints one value per file separated by NUL, in
/// argument order; a count that does not match (a file vanished mid-scan,
/// so `mdls` skipped it) cannot be attributed to paths and is discarded.
fn mdls_chunk(runner: &dyn CommandRunner, chunk: &[&Path]) -> Option<Vec<Option<i64>>> {
    let mut args: Vec<&str> = MDLS_ARGS.to_vec();
    args.extend(chunk.iter().filter_map(|p| p.to_str()));
    let outcome = run_tool(runner, MDLS, &args).ok()?;
    if !outcome.success() {
        return None;
    }
    let raw = outcome.stdout.strip_suffix('\0').unwrap_or(&outcome.stdout);
    let dates: Vec<Option<i64>> = raw.split('\0').map(parse_mdls_date).collect();
    (dates.len() == chunk.len()).then_some(dates)
}

/// Parses `2026-07-24 07:19:09 +0000` (or `(null)`) into Unix seconds.
fn parse_mdls_date(s: &str) -> Option<i64> {
    if s == MDLS_NULL {
        return None;
    }
    let (date, rest) = s.split_once(' ')?;
    let (time, offset) = rest.split_once(' ')?;
    let mut d = date.splitn(3, '-').map(|v| v.parse::<i64>());
    let (y, m, day) = (d.next()?.ok()?, d.next()?.ok()?, d.next()?.ok()?);
    let mut t = time.splitn(3, ':').map(|v| v.parse::<i64>());
    let (h, min, sec) = (t.next()?.ok()?, t.next()?.ok()?, t.next()?.ok()?);
    let sign = match offset.as_bytes().first()? {
        b'+' => 1,
        b'-' => -1,
        _ => return None,
    };
    let off = offset.get(1..5)?.parse::<i64>().ok()?;
    let off_secs = sign * ((off / 100) * 3600 + (off % 100) * 60);
    Some(days_from_civil(y, m, day) * SECS_PER_DAY + h * 3600 + min * 60 + sec - off_secs)
}

/// Days since 1970-01-01 for a proleptic Gregorian date (Hinnant's algorithm).
fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

/// Why `name` looks like private data, or `None`. Sensitive files are still
/// offered; the reason only becomes evidence.
fn sensitive_reason(name: &str) -> Option<String> {
    let lower = name.to_lowercase();
    let path = Path::new(&lower);
    let ext = path.extension().and_then(|e| e.to_str());
    if let Some(ext) = ext.filter(|e| SENSITIVE_EXTENSIONS.contains(e)) {
        return Some(format!("extension .{ext}"));
    }
    let stem = path.file_stem().and_then(|s| s.to_str());
    if ext == Some("env") || stem == Some("env") {
        return Some("environment file".to_string());
    }
    if SSH_KEY_PREFIXES.iter().any(|p| lower.starts_with(p)) {
        return Some("SSH key name".to_string());
    }
    let squashed: String = lower
        .chars()
        .filter(|c| !matches!(c, '-' | '_' | '.' | ' '))
        .collect();
    if let Some(f) = SENSITIVE_FRAGMENTS.iter().find(|f| squashed.contains(**f)) {
        return Some(format!("name contains \"{f}\""));
    }
    lower
        .split(|c: char| !c.is_alphanumeric())
        .find_map(|w| SENSITIVE_WORDS.iter().find(|s| **s == w))
        .map(|w| format!("name contains \"{w}\""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::providers::known_category_ids;
    use crate::runner::test_support::MockRunner;
    use std::fs;

    fn tempdir(label: &str) -> PathBuf {
        let mut base = std::env::temp_dir();
        base.push(format!(
            "tiny-clean-dl-{}-{}",
            label,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir_all(&base).unwrap();
        base
    }

    fn backdate(p: &Path, days: u64) {
        let old = std::time::SystemTime::now() - std::time::Duration::from_secs(days * 86_400);
        std::fs::File::open(p).unwrap().set_modified(old).unwrap();
    }

    #[test]
    fn downloads_old_id_in_known_categories() {
        assert!(known_category_ids().contains(&ID));
    }

    #[test]
    fn downloads_filters_by_age() {
        let dir = tempdir("age");
        let fresh = dir.join("fresh.txt");
        let stale = dir.join("stale.txt");
        fs::write(&fresh, b"new").unwrap();
        fs::write(&stale, b"old").unwrap();
        backdate(&stale, 60);
        let found = list_old_files(&ScanContext::unchecked(), &MockRunner::new(), &dir, 30);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].path, stale);
        let _ = crate::clean::fs_safe::remove_recursive_safe(&dir);
    }

    #[test]
    fn downloads_skips_finder_dotfiles() {
        let dir = tempdir("dot");
        for name in [".localized", ".DS_Store", "old.zip"] {
            let f = dir.join(name);
            fs::write(&f, b"x").unwrap();
            backdate(&f, 400);
        }
        let found = list_old_files(&ScanContext::unchecked(), &MockRunner::new(), &dir, 30);
        let paths: Vec<_> = found.into_iter().map(|i| i.path).collect();
        assert_eq!(paths, vec![dir.join("old.zip")]);
        let _ = crate::clean::fs_safe::remove_recursive_safe(&dir);
    }

    #[test]
    fn downloads_skips_subdirectories() {
        let dir = tempdir("subdirs");
        fs::create_dir_all(dir.join("subdir")).unwrap();
        let f = dir.join("subdir/inside.txt");
        fs::write(&f, b"x").unwrap();
        backdate(&f, 60);
        let found = list_old_files(&ScanContext::unchecked(), &MockRunner::new(), &dir, 30);
        // Subdirs not descended; even though file is old it's not flagged.
        assert!(found.is_empty());
        let _ = crate::clean::fs_safe::remove_recursive_safe(&dir);
    }

    fn days_ago_stamp(days: u64) -> String {
        let at = to_unix(SystemTime::now()) as i64 - days as i64 * SECS_PER_DAY;
        let day = at.div_euclid(SECS_PER_DAY);
        let (y, m, d) = civil_from_days(day);
        let rem = at.rem_euclid(SECS_PER_DAY);
        format!(
            "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02} +0000",
            rem / 3600,
            rem % 3600 / 60,
            rem % 60
        )
    }

    /// Inverse of `days_from_civil`, only to render fixture dates.
    fn civil_from_days(z: i64) -> (i64, i64, i64) {
        let z = z + 719_468;
        let era = z.div_euclid(146_097);
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let d = doy - (153 * mp + 2) / 5 + 1;
        let m = if mp < 10 { mp + 3 } else { mp - 9 };
        (yoe + era * 400 + i64::from(m <= 2), m, d)
    }

    /// Runner answering the one `mdls` call for `files` (in sorted order).
    fn mdls_runner(files: &[&Path], stdout: &str) -> MockRunner {
        let mut args: Vec<&str> = MDLS_ARGS.to_vec();
        args.extend(files.iter().map(|p| p.to_str().unwrap()));
        MockRunner::new().with_exit(MDLS, &args, 0, stdout)
    }

    fn old_file(dir: &Path, name: &str) -> PathBuf {
        let f = dir.join(name);
        fs::write(&f, b"x").unwrap();
        backdate(&f, 60);
        f
    }

    #[test]
    fn downloads_recently_opened_old_file_is_not_offered() {
        let dir = tempdir("opened-recent");
        let f = old_file(&dir, "Profile.pdf");
        let runner = mdls_runner(&[&f], &days_ago_stamp(2));
        let found = list_old_files(&ScanContext::unchecked(), &runner, &dir, 30);
        assert!(found.is_empty());
        let _ = crate::clean::fs_safe::remove_recursive_safe(&dir);
    }

    #[test]
    fn downloads_old_and_long_unopened_file_carries_both_evidences() {
        let dir = tempdir("opened-old");
        let f = old_file(&dir, "Profile.pdf");
        let stamp = days_ago_stamp(90);
        let runner = mdls_runner(&[&f], &stamp);
        let found = list_old_files(&ScanContext::unchecked(), &runner, &dir, 30);
        assert_eq!(found.len(), 1);
        let opened = parse_mdls_date(&stamp).unwrap();
        let modified = to_unix(fs::metadata(&f).unwrap().modified().unwrap()) as i64;
        assert_eq!(
            found[0].evidence,
            vec![
                Evidence::Modified { at: modified },
                Evidence::LastOpened { at: opened }
            ]
        );
        let _ = crate::clean::fs_safe::remove_recursive_safe(&dir);
    }

    #[test]
    fn downloads_never_opened_file_is_offered_by_mtime_alone() {
        let dir = tempdir("opened-null");
        let f = old_file(&dir, "Profile.pdf");
        let runner = mdls_runner(&[&f], MDLS_NULL);
        let found = list_old_files(&ScanContext::unchecked(), &runner, &dir, 30);
        assert_eq!(found.len(), 1);
        assert!(!found[0]
            .evidence
            .iter()
            .any(|e| matches!(e, Evidence::LastOpened { .. })));
        let _ = crate::clean::fs_safe::remove_recursive_safe(&dir);
    }

    #[test]
    fn downloads_mdls_failure_falls_back_to_mtime() {
        let dir = tempdir("mdls-fail");
        let f = old_file(&dir, "Profile.pdf");
        let mut args: Vec<&str> = MDLS_ARGS.to_vec();
        args.push(f.to_str().unwrap());
        // A failing mdls is ignored even when it printed a date.
        let runner = MockRunner::new().with_exit(MDLS, &args, 1, &days_ago_stamp(1));
        for runner in [runner, MockRunner::new()] {
            let found = list_old_files(&ScanContext::unchecked(), &runner, &dir, 30);
            assert_eq!(found.len(), 1);
            assert!(matches!(found[0].evidence[..], [Evidence::Modified { .. }]));
        }
        let _ = crate::clean::fs_safe::remove_recursive_safe(&dir);
    }

    #[test]
    fn downloads_batches_files_into_one_mdls_call() {
        let dir = tempdir("batch");
        let a = old_file(&dir, "a.pdf");
        let b = old_file(&dir, "b.pdf");
        let stdout = format!("{}\0{}", days_ago_stamp(1), days_ago_stamp(90));
        let runner = mdls_runner(&[&a, &b], &stdout);
        let found = list_old_files(&ScanContext::unchecked(), &runner, &dir, 30);
        assert_eq!(runner.output_calls.lock().unwrap().len(), 1);
        let paths: Vec<_> = found.into_iter().map(|i| i.path).collect();
        assert_eq!(paths, vec![b]);
        let _ = crate::clean::fs_safe::remove_recursive_safe(&dir);
    }

    #[test]
    fn downloads_mismatched_mdls_output_is_ignored() {
        let dir = tempdir("mismatch");
        let a = old_file(&dir, "a.pdf");
        let b = old_file(&dir, "b.pdf");
        // One value for two files: positions cannot be trusted.
        let runner = mdls_runner(&[&a, &b], &days_ago_stamp(1));
        let found = list_old_files(&ScanContext::unchecked(), &runner, &dir, 30);
        assert_eq!(found.len(), 2);
        let _ = crate::clean::fs_safe::remove_recursive_safe(&dir);
    }

    #[test]
    fn mdls_dates_parse_to_unix_seconds() {
        assert_eq!(
            parse_mdls_date("2026-07-24 07:19:09 +0000"),
            Some(1_784_877_549)
        );
        assert_eq!(
            parse_mdls_date("2026-07-24 14:19:09 +0700"),
            Some(1_784_877_549)
        );
        assert_eq!(parse_mdls_date("1969-12-31 23:59:59 +0000"), Some(-1));
        assert_eq!(parse_mdls_date(MDLS_NULL), None);
        assert_eq!(parse_mdls_date("garbage"), None);
    }

    #[test]
    fn sensitive_names_are_flagged_with_a_reason() {
        let cases = [
            ("github-recovery-codes.txt", "name contains \"recovery\""),
            ("payos-backupcode.txt", "name contains \"backupcode\""),
            ("Backup Codes.txt", "name contains \"backupcode\""),
            ("backup-code.png", "name contains \"backupcode\""),
            ("my_Password_list.csv", "name contains \"password\""),
            ("stripe API-Key.txt", "name contains \"apikey\""),
            ("github-2fa-codes.txt", "name contains \"2fa\""),
            ("seed-phrase.txt", "name contains \"seed\""),
            ("github_token.txt", "name contains \"token\""),
            ("developerID_application.cer", "extension .cer"),
            ("Server.PEM", "extension .pem"),
            ("vault.kdbx", "extension .kdbx"),
            ("work.ovpn", "extension .ovpn"),
            ("env.txt", "environment file"),
            ("prod.env", "environment file"),
            ("id_ed25519", "SSH key name"),
            ("id_rsa.pub", "SSH key name"),
        ];
        for (name, reason) in cases {
            assert_eq!(sensitive_reason(name).as_deref(), Some(reason), "{name}");
        }
    }

    #[test]
    fn ordinary_names_are_not_flagged() {
        // `seed` and `token` are word matches, so `seeds.csv` and
        // `tokenizer.json` pass; `2fa` inside a hex name passes too.
        let names = [
            "Profile.pdf",
            "Linear-1.32.4-universal.dmg",
            "environment-map.png",
            "envelope.pdf",
            "seeds.csv",
            "tokenizer.json",
            "0cb2e5a2fa10.MOV",
            "keynote.key.zip",
            "monkey.png",
        ];
        for name in names {
            assert_eq!(sensitive_reason(name), None, "{name}");
        }
    }

    #[test]
    fn downloads_sensitive_file_is_still_offered_with_evidence() {
        let dir = tempdir("sensitive");
        old_file(&dir, "github-recovery-codes.txt");
        old_file(&dir, "Profile.pdf");
        let found = list_old_files(&ScanContext::unchecked(), &MockRunner::new(), &dir, 30);
        assert_eq!(found.len(), 2);
        let flagged: Vec<_> = found
            .iter()
            .filter(|i| {
                i.evidence
                    .iter()
                    .any(|e| matches!(e, Evidence::Sensitive { .. }))
            })
            .map(|i| i.path.clone())
            .collect();
        assert_eq!(flagged, vec![dir.join("github-recovery-codes.txt")]);
        let _ = crate::clean::fs_safe::remove_recursive_safe(&dir);
    }
}
