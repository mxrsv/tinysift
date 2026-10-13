//! Duplicate file detection.
//!
//! Phase 1 (always): group files by `(size, basename)`. Two files that share
//! both the exact byte size and the exact filename are very likely duplicates
//! produced by re-downloads, copies, or backups.
//!
//! Phase 2 (`--hash`): verify each candidate group by hashing the file
//! contents. Files with the same content hash form a confirmed group; files
//! that hash differently are dropped from the group. Hashing uses the std
//! library `DefaultHasher` (SipHash-2-4) which is non-cryptographic but
//! collision-resistant enough for ad-hoc dedup.

use std::collections::hash_map::DefaultHasher;
use std::collections::HashMap;
use std::fs::File;
use std::hash::Hasher as _;
use std::io::{BufReader, Read};
use std::path::PathBuf;

use super::types::FileEntry;

#[derive(Debug, Clone)]
pub struct DuplicateGroup {
    pub key: String,
    pub size: u64,
    pub name: String,
    pub paths: Vec<PathBuf>,
    pub verified_by_hash: bool,
}

impl DuplicateGroup {
    pub fn wasted_bytes(&self) -> u64 {
        if self.paths.len() <= 1 {
            return 0;
        }
        self.size.saturating_mul((self.paths.len() as u64) - 1)
    }
}

pub fn find(files: &[FileEntry], verify_with_hash: bool) -> Vec<DuplicateGroup> {
    let candidates = group_by_size_and_name(files);
    let mut out = Vec::new();
    for ((size, name), paths) in candidates {
        if paths.len() < 2 {
            continue;
        }
        if verify_with_hash {
            for sub in split_by_hash(&paths) {
                if sub.len() < 2 {
                    continue;
                }
                out.push(DuplicateGroup {
                    key: format!("{}:{}", size, name),
                    size,
                    name: name.clone(),
                    paths: sub,
                    verified_by_hash: true,
                });
            }
        } else {
            out.push(DuplicateGroup {
                key: format!("{}:{}", size, name),
                size,
                name,
                paths,
                verified_by_hash: false,
            });
        }
    }
    out.sort_by_key(|g| std::cmp::Reverse(g.wasted_bytes()));
    out
}

fn group_by_size_and_name(files: &[FileEntry]) -> HashMap<(u64, String), Vec<PathBuf>> {
    let mut buckets: HashMap<(u64, String), Vec<PathBuf>> = HashMap::new();
    for f in files {
        if f.size == 0 {
            continue;
        }
        let name = match f.path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n.to_string(),
            None => continue,
        };
        buckets
            .entry((f.size, name))
            .or_default()
            .push(f.path.clone());
    }
    buckets
}

fn split_by_hash(paths: &[PathBuf]) -> Vec<Vec<PathBuf>> {
    let mut by_hash: HashMap<u64, Vec<PathBuf>> = HashMap::new();
    for p in paths {
        match hash_file(p) {
            Ok(h) => by_hash.entry(h).or_default().push(p.clone()),
            Err(_) => continue,
        }
    }
    by_hash.into_values().collect()
}

fn hash_file(path: &PathBuf) -> std::io::Result<u64> {
    let file = File::open(path)?;
    let mut reader = BufReader::with_capacity(64 * 1024, file);
    let mut hasher = DefaultHasher::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = reader.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.write(&buf[..n]);
    }
    Ok(hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn entry(path: &str, size: u64) -> FileEntry {
        FileEntry {
            path: PathBuf::from(path),
            size,
            age_secs: 0,
        }
    }

    #[test]
    fn groups_files_with_matching_size_and_name() {
        let files = vec![
            entry("/a/report.pdf", 1024),
            entry("/b/report.pdf", 1024),
            entry("/c/report.pdf", 2048),
            entry("/a/notes.txt", 50),
        ];
        let groups = find(&files, false);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].size, 1024);
        assert_eq!(groups[0].name, "report.pdf");
        assert_eq!(groups[0].paths.len(), 2);
        assert!(!groups[0].verified_by_hash);
    }

    #[test]
    fn ignores_empty_files() {
        let files = vec![entry("/a/x", 0), entry("/b/x", 0)];
        let groups = find(&files, false);
        assert!(groups.is_empty());
    }

    #[test]
    fn wasted_bytes_excludes_one_kept_copy() {
        let g = DuplicateGroup {
            key: "k".into(),
            size: 100,
            name: "x".into(),
            paths: vec![PathBuf::from("a"), PathBuf::from("b"), PathBuf::from("c")],
            verified_by_hash: false,
        };
        assert_eq!(g.wasted_bytes(), 200);
    }

    #[test]
    fn singletons_have_no_wasted_bytes() {
        let g = DuplicateGroup {
            key: "k".into(),
            size: 100,
            name: "x".into(),
            paths: vec![PathBuf::from("a")],
            verified_by_hash: false,
        };
        assert_eq!(g.wasted_bytes(), 0);
    }
}
