use crate::engine_error;
use crate::error::Result;
use std::path::PathBuf;
use std::process::Command;

use super::CleanProvider;
use crate::clean::fs_safe::dir_size_checked;
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};

const ID: &str = "trash";
const LABEL: &str = "User Trash";

pub struct TrashProvider;

impl CleanProvider for TrashProvider {
    fn id(&self) -> &'static str {
        ID
    }
    fn label(&self) -> &'static str {
        LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Items already in the Trash; emptying it is permanent".into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Destructive
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let h = match std::env::var_os("HOME").map(PathBuf::from) {
            Some(h) => h,
            None => return Ok(Vec::new()),
        };
        let root = h.join(".Trash");
        if !root.exists() {
            return Ok(Vec::new());
        }
        let size = dir_size_checked(&root, ctx);
        Ok(vec![CleanItem {
            category_id: ID.to_string(),
            category_label: LABEL.to_string(),
            path: root,
            size,
            risk: RiskLevel::Destructive,
            evidence: Vec::new(),
        }])
    }

    /// Trash provider rejects every action except `EmptyTrash`. The
    /// orchestrator (`execute.rs`) is responsible for producing this
    /// mapping; tests here pin the rejection behaviour.
    fn execute(&self, _items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        match action {
            ExecAction::EmptyTrash => empty_trash(),
            ExecAction::Trash => Err(engine_error!(
                "trash provider rejects ExecAction::Trash — empty Trash via Hard delete only"
            )),
            ExecAction::HardDelete => Err(engine_error!(
                "trash provider rejects ExecAction::HardDelete — execute.rs must map UI HardDelete → ExecAction::EmptyTrash for trash"
            )),
        }
    }
}

fn empty_trash() -> Result<ExecReport> {
    let script = "tell application \"Finder\" to empty trash";
    let output = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .output()
        .map_err(|e| engine_error!("failed to spawn osascript: {}", e))?;
    let mut report = ExecReport::default();
    if output.status.success() {
        if let Some(home) = std::env::var_os("HOME").map(PathBuf::from) {
            report.removed_paths.push(home.join(".Trash"));
        }
        Ok(report)
    } else {
        let err = String::from_utf8_lossy(&output.stderr).trim().to_string();
        Err(engine_error!("Finder empty trash failed: {}", err))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_trash_action() {
        let p = TrashProvider;
        let err = p.execute(&[], ExecAction::Trash).unwrap_err();
        assert!(err.to_string().contains("rejects ExecAction::Trash"));
    }

    #[test]
    fn rejects_hard_delete() {
        let p = TrashProvider;
        let err = p.execute(&[], ExecAction::HardDelete).unwrap_err();
        assert!(err.to_string().contains("rejects ExecAction::HardDelete"));
    }
}
