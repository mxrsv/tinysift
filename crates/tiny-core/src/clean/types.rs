use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RiskLevel {
    Safe,
    Review,
    Destructive,
}

impl RiskLevel {
    pub fn badge(&self) -> &'static str {
        match self {
            RiskLevel::Safe => "safe",
            RiskLevel::Review => "review",
            RiskLevel::Destructive => "destructive",
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[allow(dead_code)]
#[serde(rename_all = "camelCase")]
pub struct CleanItem {
    pub category_id: String,
    pub category_label: String,
    pub path: PathBuf,
    pub size: u64,
    pub risk: RiskLevel,
    /// Facts showing why this path was offered; the desktop shows them
    /// beside the path. Empty means the category rule alone selected it.
    #[serde(default)]
    pub evidence: Vec<Evidence>,
}

/// One checkable fact behind a candidate. Times are Unix seconds.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum Evidence {
    /// The path's own modification time.
    Modified { at: i64 },
    /// A project manifest (`Cargo.toml`, `package.json`, ...) and its mtime.
    ManifestModified { file: String, at: i64 },
    /// Last commit of the git work tree holding the project.
    LastCommit { at: i64 },
    /// The git work tree has no uncommitted changes.
    WorkTreeClean,
    /// The project is not inside a git work tree.
    NotGitRepo,
    /// The folder holds `pyvenv.cfg`, so it is a virtualenv.
    VenvMarker,
    /// Spotlight's last-opened date.
    LastOpened { at: i64 },
    /// The app that owns the path; it was not running at scan time.
    OwningApp { name: String },
    /// No owning app could be resolved.
    OwningAppUnknown,
    /// The name looks like private data (keys, recovery codes, ...).
    Sensitive { reason: String },
    /// `xcrun simctl` reports the simulator device unavailable, with its
    /// `availabilityError` (empty when simctl gave none).
    SimulatorUnavailable { name: String, reason: String },
}

/// How a category's items come back after they are moved to the Trash.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub enum ComesBack {
    /// A build or install command recreates them.
    Rebuild { command: &'static str },
    /// The tool or app downloads them again when needed.
    Redownload,
    /// The app or system writes them again on its own.
    AppRecreates,
    /// Nothing recreates them; only the Trash can restore them.
    TrashOnly,
    /// Removal is permanent (snapshots, Empty Trash).
    NotRecoverable,
}

/// What reaches a provider's execute(). Three semantics, distinct on
/// purpose so providers can't accidentally conflate them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ExecAction {
    Trash,
    HardDelete,
    EmptyTrash,
}

/// What the user sees in the action menu. Mapped to per-provider
/// ExecAction by execute.rs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CleanAction {
    Trash,
    HardDelete,
    DryRun,
    Cancel,
}

#[derive(Debug, Default, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecReport {
    pub removed_paths: Vec<PathBuf>,
    pub failed: Vec<(PathBuf, String)>,
    pub skipped_running_app: Option<String>,
}

impl ExecReport {
    pub fn merge(&mut self, other: ExecReport) {
        self.removed_paths.extend(other.removed_paths);
        self.failed.extend(other.failed);
        if self.skipped_running_app.is_none() {
            self.skipped_running_app = other.skipped_running_app;
        }
    }
}
