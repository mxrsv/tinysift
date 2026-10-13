//! Per-extension aggregation: count + total bytes per file extension.

use std::collections::HashMap;
use std::path::Path;

use super::types::FileEntry;

#[derive(Debug, Clone)]
pub struct ExtStat {
    pub ext: String,
    pub count: u64,
    pub total_size: u64,
}

pub fn aggregate(files: &[FileEntry]) -> Vec<ExtStat> {
    let mut buckets: HashMap<String, (u64, u64)> = HashMap::new();
    for f in files {
        let ext = Path::new(&f.path)
            .extension()
            .and_then(|e| e.to_str())
            .map(|e| e.to_lowercase())
            .unwrap_or_else(|| "(no ext)".to_string());
        let entry = buckets.entry(ext).or_insert((0, 0));
        entry.0 += 1;
        entry.1 += f.size;
    }

    let mut out: Vec<ExtStat> = buckets
        .into_iter()
        .map(|(ext, (count, total_size))| ExtStat {
            ext,
            count,
            total_size,
        })
        .collect();
    out.sort_by_key(|a| std::cmp::Reverse(a.total_size));
    out
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
    fn aggregates_by_extension_and_sorts_by_total_size_desc() {
        let files = vec![
            entry("/a/big.zip", 1000),
            entry("/a/small.zip", 100),
            entry("/a/note.txt", 50),
            entry("/a/README", 30),
        ];
        let stats = aggregate(&files);
        assert_eq!(stats.len(), 3);
        assert_eq!(stats[0].ext, "zip");
        assert_eq!(stats[0].count, 2);
        assert_eq!(stats[0].total_size, 1100);
        assert_eq!(stats[1].ext, "txt");
        assert_eq!(stats[2].ext, "(no ext)");
        assert_eq!(stats[2].count, 1);
    }

    #[test]
    fn extension_match_is_case_insensitive() {
        let files = vec![entry("/a.JPG", 10), entry("/b.jpg", 20)];
        let stats = aggregate(&files);
        assert_eq!(stats.len(), 1);
        assert_eq!(stats[0].ext, "jpg");
        assert_eq!(stats[0].count, 2);
    }
}
