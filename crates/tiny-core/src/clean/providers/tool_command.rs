//! The desktop's tool commands: categories whose cleanup is their tool's own
//! command (`uv cache prune`, `pnpm store prune`, `xcrun simctl runtime
//! delete`). The CLI runs the same commands through `CleanProvider::execute`
//! with the unbounded `CommandRunner::run`; the desktop runs them through the
//! bounded `CommandRunner::output`, one item at a time, after its checks.

use std::time::Duration;

use crate::runner::CommandRunner;

/// How a category's own tool removes an item on the desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopTool {
    /// The tool prunes the cache folder the item names. Freed bytes are the
    /// folder's size before minus after. In Trash mode the folder is moved to
    /// the Trash instead, like any path.
    Prune,
    /// The tool removes the item in the background (`simctl runtime
    /// delete`), so the removal is only requested. There is no Trash route.
    Request,
}

/// Bound for one desktop tool command: pruning a large cache can take
/// minutes.
pub const TOOL_COMMAND_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Runs `bin args...` for the desktop: resolved to an absolute path, stdin
/// closed, no shell, never elevated, killed after `TOOL_COMMAND_TIMEOUT`. A
/// non-zero exit is an error naming the tool's first line of stderr.
pub(crate) fn run_tool_command(
    runner: &dyn CommandRunner,
    bin: &str,
    args: &[&str],
) -> Result<(), String> {
    let outcome = runner
        .output(bin, args, TOOL_COMMAND_TIMEOUT)
        .map_err(|e| e.to_string())?;
    if outcome.success() {
        return Ok(());
    }
    let reason = outcome
        .stderr
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or("no error output");
    Err(format!("{bin} {} failed: {reason}", args.join(" ")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runner::test_support::MockRunner;
    use crate::runner::CommandError;

    #[test]
    fn only_uv_pnpm_and_simulator_runtimes_have_a_desktop_tool() {
        use crate::clean::providers::all_providers_with;
        use std::sync::Arc;
        let runner: Arc<dyn CommandRunner> =
            Arc::new(MockRunner::new().with_which("uv").with_which("pnpm"));
        let options = crate::options::CleanOptions::default();
        let tools: Vec<(&str, DesktopTool)> = all_providers_with(&options, runner)
            .iter()
            .filter_map(|p| p.desktop_tool().map(|tool| (p.id(), tool)))
            .collect();
        assert_eq!(
            tools,
            [
                ("pnpm", DesktopTool::Prune),
                ("uv", DesktopTool::Prune),
                ("simulator-runtimes", DesktopTool::Request),
            ],
            "simulator-runtime-images stays report only (XcodeNotInstalled)"
        );
    }

    #[test]
    fn a_tool_command_reports_its_failure_and_never_hides_one() {
        let runner = MockRunner::new()
            .with_exit("uv", &["cache", "prune"], 0, "")
            .with_exit("pnpm", &["store", "prune"], 1, "");
        assert_eq!(run_tool_command(&runner, "uv", &["cache", "prune"]), Ok(()));
        assert_eq!(
            run_tool_command(&runner, "pnpm", &["store", "prune"]),
            Err("pnpm store prune failed: no error output".into())
        );
        let missing = run_tool_command(&runner, "xcrun", &["simctl"]);
        assert_eq!(
            missing,
            Err(CommandError::NotFound("xcrun".into()).to_string())
        );
    }
}
