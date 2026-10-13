//! Sorting helpers shared between text and JSON reporters.

use crate::options::ScanSort;

use super::types::FileEntry;

pub fn sort_files(files: &mut [FileEntry], by: ScanSort) {
    match by {
        ScanSort::Size => files.sort_by_key(|a| std::cmp::Reverse(a.size)),
        ScanSort::Age => files.sort_by_key(|a| std::cmp::Reverse(a.age_secs)),
        ScanSort::Path => files.sort_by(|a, b| a.path.cmp(&b.path)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn entry(path: &str, size: u64, age: u64) -> FileEntry {
        FileEntry {
            path: PathBuf::from(path),
            size,
            age_secs: age,
        }
    }

    #[test]
    fn sort_by_size_desc() {
        let mut files = vec![entry("/a", 1, 10), entry("/b", 100, 5), entry("/c", 50, 1)];
        sort_files(&mut files, ScanSort::Size);
        assert_eq!(files[0].size, 100);
        assert_eq!(files[1].size, 50);
        assert_eq!(files[2].size, 1);
    }

    #[test]
    fn sort_by_age_desc() {
        let mut files = vec![entry("/a", 1, 10), entry("/b", 1, 100), entry("/c", 1, 5)];
        sort_files(&mut files, ScanSort::Age);
        assert_eq!(files[0].age_secs, 100);
        assert_eq!(files[2].age_secs, 5);
    }

    #[test]
    fn sort_by_path_asc() {
        let mut files = vec![entry("/c", 1, 0), entry("/a", 1, 0), entry("/b", 1, 0)];
        sort_files(&mut files, ScanSort::Path);
        assert_eq!(files[0].path, PathBuf::from("/a"));
        assert_eq!(files[2].path, PathBuf::from("/c"));
    }
}
