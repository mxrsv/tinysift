use std::path::PathBuf;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ScanOptions {
    pub min_size_mb: u64,

    pub older_than_days: u64,

    pub path: Vec<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ScanSort {
    /// Largest files first
    Size,
    /// Oldest files first
    Age,
    /// Lexicographic path order
    Path,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CleanOptions {
    pub category: Vec<String>,

    pub include_review: bool,

    pub include_destructive: bool,

    pub idle_days: u64,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UninstallOptions {
    pub shallow: bool,

    pub leftovers_only: bool,

    pub force: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SortBy {
    /// Least recently used first (default)
    LastUsed,
    /// Largest size first
    Size,
    /// Alphabetical
    Name,
}

impl Default for CleanOptions {
    fn default() -> Self {
        Self {
            category: Vec::new(),
            include_review: false,
            include_destructive: false,
            idle_days: 30,
        }
    }
}
impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            min_size_mb: 100,
            older_than_days: 90,
            path: Vec::new(),
        }
    }
}
