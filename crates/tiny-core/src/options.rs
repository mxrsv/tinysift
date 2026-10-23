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

    /// List the build output (`target/`, `node_modules`) of every project,
    /// not only projects idle for `idle_days`. The desktop sets it; the CLI
    /// never does.
    #[serde(default)]
    pub all_build_output: bool,

    /// Scan the user's Trash (`trash`) even without `include_destructive`,
    /// so the desktop can offer emptying it. The CLI never sets it.
    #[serde(default)]
    pub include_trash: bool,
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
            all_build_output: false,
            include_trash: false,
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
