use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub operation: String,
    pub completed: u64,
    pub total: Option<u64>,
    pub message: String,
    /// Scan only: bytes found so far in this run, each path counted once.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub found_bytes: Option<u64>,
    /// Scan only: the category finished most recently before this event.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub category: Option<String>,
    /// Scan only: the ID of the category being scanned now; `None` once all are done.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub current_category: Option<String>,
}
pub type ProgressCallback<'a> = Option<&'a (dyn Fn(Progress) + Send + Sync)>;
pub fn report(
    callback: ProgressCallback<'_>,
    operation: &str,
    completed: u64,
    total: Option<u64>,
    message: &str,
) {
    if let Some(callback) = callback {
        callback(Progress {
            operation: operation.into(),
            completed,
            total,
            message: message.into(),
            found_bytes: None,
            category: None,
            current_category: None,
        });
    }
}

/// `report` for a scan: adds what was found so far, the last finished category
/// and the category being scanned now.
pub fn report_scan(
    callback: ProgressCallback<'_>,
    completed: u64,
    total: u64,
    message: &str,
    found_bytes: u64,
    category: Option<&str>,
    current_category: Option<&str>,
) {
    if let Some(callback) = callback {
        callback(Progress {
            operation: "scan".into(),
            completed,
            total: Some(total),
            message: message.into(),
            found_bytes: Some(found_bytes),
            category: category.map(Into::into),
            current_category: current_category.map(Into::into),
        });
    }
}
