use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::error::Result;

use super::{execute_per_item, root_as_item, CleanProvider};
use crate::clean::process::command_line_running;
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};
use crate::runner::CommandRunner;

const ID: &str = "gradle-maven";
const LABEL: &str = "Gradle caches";

/// Maven's local repository (`~/.m2/repository`) is not offered: besides
/// downloads it holds what `mvn install` put there, which no server has.
const GRADLE_SUBDIRS: &[&str] = &[".gradle/caches", ".gradle/daemon"];
/// `pgrep -f` pattern for a running Gradle: the daemon, the client and the
/// wrapper all run as `java` with one of these classes. A daemon outlives
/// its build (three idle hours by default) and holds the caches open.
const GRADLE_COMMAND_LINE: &str = r"org\.gradle\.(launcher|wrapper)\.";

fn home() -> Option<PathBuf> {
    std::env::var_os("HOME").map(PathBuf::from)
}

pub struct GradleMaven {
    runner: Arc<dyn CommandRunner>,
}

impl GradleMaven {
    pub fn with_runner(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }
}

impl CleanProvider for GradleMaven {
    fn id(&self) -> &'static str {
        ID
    }
    fn label(&self) -> &'static str {
        LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Gradle caches re-downloaded on the next build".into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    fn desktop_trash_paths(&self) -> bool {
        true
    }
    fn available(&self) -> bool {
        let h = match home() {
            Some(h) => h,
            None => return false,
        };
        GRADLE_SUBDIRS.iter().any(|s| h.join(s).exists())
    }
    fn discover(&self, ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        Ok(home().map_or_else(Vec::new, |h| gradle_caches(ctx, &h)))
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        execute_per_item(items, action, ID)
    }
    fn check_item(&self, _path: &Path) -> std::result::Result<(), String> {
        match command_line_running(self.runner.as_ref(), GRADLE_COMMAND_LINE) {
            Ok(false) => Ok(()),
            Ok(true) => Err("Gradle is running".into()),
            Err(e) => Err(format!("running-Gradle check failed: {e}")),
        }
    }
}

fn gradle_caches(ctx: &ScanContext<'_>, home: &Path) -> Vec<CleanItem> {
    GRADLE_SUBDIRS
        .iter()
        .flat_map(|sub| root_as_item(ctx, &home.join(sub), ID, LABEL, RiskLevel::Review))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::providers::known_category_ids;

    #[test]
    fn gradle_maven_id_in_known_categories() {
        assert!(known_category_ids().contains(&ID));
    }

    /// RP-04: `mvn install` puts the user's own builds in `~/.m2/repository`.
    #[test]
    fn maven_s_local_repository_is_never_offered() {
        let home = tempfile::tempdir().unwrap();
        for sub in [
            ".gradle/caches/modules-2",
            ".gradle/daemon/8.10",
            ".m2/repository/com",
        ] {
            std::fs::create_dir_all(home.path().join(sub)).unwrap();
        }
        let items = gradle_caches(&ScanContext::unchecked(), home.path());
        let paths: Vec<_> = items.iter().map(|i| i.path.clone()).collect();
        assert_eq!(
            paths,
            [".gradle/caches", ".gradle/daemon"].map(|sub| home.path().join(sub))
        );
    }

    /// RP-06: a running daemon holds the caches open.
    #[test]
    fn caches_are_refused_while_gradle_runs_or_when_that_is_unknown() {
        use crate::runner::test_support::MockRunner;
        let gradle = |status| {
            GradleMaven::with_runner(Arc::new(MockRunner::new().with_exit(
                "/usr/bin/pgrep",
                &["-a", "-f", "--", GRADLE_COMMAND_LINE],
                status,
                "",
            )))
        };
        let path = Path::new("/Users/me/.gradle/caches");
        assert_eq!(gradle(1).check_item(path), Ok(()));
        assert_eq!(gradle(0).check_item(path), Err("Gradle is running".into()));
        assert!(gradle(2)
            .check_item(path)
            .unwrap_err()
            .starts_with("running-Gradle check failed"));
    }

    #[test]
    fn the_running_gradle_pattern_matches_what_gradle_runs_as_and_nothing_else() {
        // `pgrep -f` and `grep -E` take the same extended regular expressions.
        let matches = |line: &str| {
            let mut grep = std::process::Command::new("/usr/bin/grep")
                .args(["-E", "-q", "--", GRADLE_COMMAND_LINE])
                .stdin(std::process::Stdio::piped())
                .spawn()
                .unwrap();
            std::io::Write::write_all(&mut grep.stdin.take().unwrap(), line.as_bytes()).unwrap();
            grep.wait().unwrap().success()
        };
        for running in [
            "/usr/bin/java -Xmx2g -cp /Users/me/.gradle/wrapper/dists/gradle-8.10-bin/x/gradle-8.10/lib/gradle-daemon-main-8.10.jar org.gradle.launcher.daemon.bootstrap.GradleDaemon 8.10",
            "java -classpath /Users/me/app/gradle/wrapper/gradle-wrapper.jar org.gradle.wrapper.GradleWrapperMain assembleDebug",
            "java -cp /opt/homebrew/Cellar/gradle/8.10/libexec/lib/gradle-launcher-8.10.jar org.gradle.launcher.GradleMain build",
        ] {
            assert!(matches(running), "{running}");
        }
        for other in [
            "/usr/bin/vim build.gradle",
            "/Users/me/.gradle/caches/modules-2",
            "java -jar app.jar --name=org.gradleish.launcher",
        ] {
            assert!(!matches(other), "{other}");
        }
    }
}
