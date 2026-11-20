use std::path::Path;

use crate::error::Result;

use super::{execute_per_item, CleanProvider};
use crate::clean::fs_safe::{dir_size_checked, list_children};
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};

const ID: &str = "user-logs";
const LABEL: &str = "User logs";
const LOGS_DIR: &str = "Library/Logs";
/// Folders of `~/Library/Logs` that macOS owns and saves its reports into.
/// The folder itself is never offered: `crash-reports` lists the reports
/// inside `DiagnosticReports`, so they still go, and the folder stays for
/// the next one.
const SYSTEM_FOLDERS: &[&str] = &["DiagnosticReports"];

fn is_system_folder(path: &Path) -> bool {
    path.file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            SYSTEM_FOLDERS
                .iter()
                .any(|folder| name.eq_ignore_ascii_case(folder))
        })
}

/// Every entry directly inside `root` except the folders macOS owns.
/// Returns empty when `root` does not exist.
fn log_entries(ctx: &ScanContext<'_>, root: &Path) -> Vec<CleanItem> {
    list_children(root, ctx)
        .into_iter()
        .filter(|path| !is_system_folder(path))
        .map(|path| {
            let size = dir_size_checked(&path, ctx);
            CleanItem {
                category_id: ID.to_string(),
                category_label: LABEL.to_string(),
                path,
                size,
                risk: RiskLevel::Safe,
                evidence: Vec::new(),
            }
        })
        .collect()
}

pub struct UserLogs;

impl CleanProvider for UserLogs {
    fn id(&self) -> &'static str {
        ID
    }

    fn label(&self) -> &'static str {
        LABEL
    }
    fn inclusion_reason(&self) -> String {
        "App logs kept only for troubleshooting".into()
    }

    fn risk(&self) -> RiskLevel {
        RiskLevel::Safe
    }
    fn desktop_trash_paths(&self) -> bool {
        true
    }

    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        let home = match std::env::var_os("HOME") {
            Some(h) => std::path::PathBuf::from(h),
            None => return Ok(Vec::new()),
        };
        Ok(log_entries(ctx, &home.join(LOGS_DIR)))
    }

    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_per_item(items, action, ID)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_empty_trash_action() {
        let p = UserLogs;
        let err = p.execute(&[], ExecAction::EmptyTrash).unwrap_err();
        assert!(
            err.to_string().contains("does not accept EmptyTrash"),
            "expected EmptyTrash rejection, got: {err}"
        );
    }

    #[test]
    fn discover_returns_empty_when_logs_dir_missing() {
        // We don't manipulate $HOME here; instead exercise the helper
        // directly with a guaranteed-missing path.
        let items = log_entries(
            &ScanContext::unchecked(),
            Path::new("/tmp/__tiny_clean_logs_missing__"),
        );
        assert!(items.is_empty());
    }

    fn tempdir(label: &str) -> std::path::PathBuf {
        let mut base = std::env::temp_dir();
        base.push(format!(
            "tiny-clean-logs-{}-{}",
            label,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&base).unwrap();
        base
    }

    #[test]
    fn the_crash_report_folder_itself_is_never_offered() {
        let root = tempdir("system-folder");
        std::fs::create_dir_all(root.join("DiagnosticReports")).unwrap();
        std::fs::write(root.join("DiagnosticReports/app.ips"), b"report").unwrap();
        std::fs::create_dir_all(root.join("SomeApp")).unwrap();
        std::fs::write(root.join("SomeApp/run.log"), b"log").unwrap();
        std::fs::write(root.join("tool.log"), b"log").unwrap();

        let items = log_entries(&ScanContext::unchecked(), &root);
        let mut names: Vec<_> = items
            .iter()
            .map(|item| item.path.file_name().unwrap().to_str().unwrap().to_string())
            .collect();
        names.sort();

        assert_eq!(names, ["SomeApp", "tool.log"]);
        assert!(items.iter().all(|item| item.category_id == ID));
        let _ = crate::clean::fs_safe::remove_recursive_safe(&root);
    }

    #[test]
    fn a_system_folder_is_matched_whatever_its_case() {
        assert!(is_system_folder(Path::new("/l/DiagnosticReports")));
        assert!(is_system_folder(Path::new("/l/diagnosticreports")));
        assert!(!is_system_folder(Path::new("/l/DiagnosticReports.old")));
        assert!(!is_system_folder(Path::new("/l/SomeApp")));
    }
}
