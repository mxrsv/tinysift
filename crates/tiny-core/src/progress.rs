use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Progress {
    pub operation: String,
    pub completed: u64,
    pub total: Option<u64>,
    pub message: String,
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
        });
    }
}
