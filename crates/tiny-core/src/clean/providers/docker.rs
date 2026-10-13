use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::engine_error;
use crate::error::Result;

use super::{run_tool, CleanProvider};
use crate::clean::runner::{CommandRunner, RealRunner};
use crate::clean::scan_context::ScanContext;
use crate::clean::types::{CleanItem, ExecAction, ExecReport, RiskLevel};

const ID: &str = "docker";
const LABEL: &str = "Docker images/build cache";
const VOLUMES_ID: &str = "docker-volumes";
const VOLUMES_LABEL: &str = "Docker volumes (data)";
const APP: &str = "Docker Desktop";

/// Synthetic placeholders so `CleanItem.path` stays meaningful in the UI
/// even though there's no filesystem entry to remove. `execute()` reads
/// these back to decide what to prune.
const PLACEHOLDER_IMAGES: &str = "<docker:images>";
const PLACEHOLDER_BUILD: &str = "<docker:build-cache>";
const PLACEHOLDER_VOLUMES: &str = "<docker:volumes>";

const CACHE_KINDS: &[DfKind] = &[DfKind::Images, DfKind::BuildCache];
const VOLUME_KINDS: &[DfKind] = &[DfKind::Volumes];

/// Images and build cache: Docker pulls or rebuilds them on demand.
pub struct Docker {
    runner: Arc<dyn CommandRunner>,
}

impl Docker {
    pub fn new() -> Self {
        Self {
            runner: Arc::new(RealRunner),
        }
    }
    pub fn with_runner(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }
}

impl Default for Docker {
    fn default() -> Self {
        Self::new()
    }
}

impl CleanProvider for Docker {
    fn id(&self) -> &'static str {
        ID
    }
    fn label(&self) -> &'static str {
        LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Unused Docker images and build cache reported by docker system df".into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Review
    }
    fn requires_app_quit(&self) -> Option<&'static str> {
        Some(APP)
    }
    fn available(&self) -> bool {
        self.runner.which("docker")
    }
    fn required_tool(&self) -> Option<&'static str> {
        Some("docker")
    }
    fn discover(&self, _ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        discover_kinds(
            self.runner.as_ref(),
            CACHE_KINDS,
            ID,
            LABEL,
            RiskLevel::Review,
        )
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        prune(self.runner.as_ref(), items, action, CACHE_KINDS, ID)
    }
}

/// Volumes hold container data (databases, uploads) that nothing brings
/// back, so they are a separate `destructive` category: listed only with
/// `--include-destructive` or `--category docker-volumes`.
pub struct DockerVolumes {
    runner: Arc<dyn CommandRunner>,
}

impl DockerVolumes {
    pub fn with_runner(runner: Arc<dyn CommandRunner>) -> Self {
        Self { runner }
    }
}

impl CleanProvider for DockerVolumes {
    fn id(&self) -> &'static str {
        VOLUMES_ID
    }
    fn label(&self) -> &'static str {
        VOLUMES_LABEL
    }
    fn inclusion_reason(&self) -> String {
        "Docker volumes no container uses, reported by docker system df; they hold container data"
            .into()
    }
    fn risk(&self) -> RiskLevel {
        RiskLevel::Destructive
    }
    fn requires_app_quit(&self) -> Option<&'static str> {
        Some(APP)
    }
    fn available(&self) -> bool {
        self.runner.which("docker")
    }
    fn required_tool(&self) -> Option<&'static str> {
        Some("docker")
    }
    fn discover(&self, _ctx: &ScanContext<'_>) -> Result<Vec<CleanItem>> {
        discover_kinds(
            self.runner.as_ref(),
            VOLUME_KINDS,
            VOLUMES_ID,
            VOLUMES_LABEL,
            RiskLevel::Destructive,
        )
    }
    fn execute(&self, items: &[CleanItem], action: ExecAction) -> Result<ExecReport> {
        prune(
            self.runner.as_ref(),
            items,
            action,
            VOLUME_KINDS,
            VOLUMES_ID,
        )
    }
}

/// Lists the resource types in `kinds` from `docker system df --format
/// '{{json .}}'`, which emits one JSON object per type (Images,
/// Containers, Local Volumes, Build Cache). Daemon down → non-zero exit →
/// an error the checked discovery reports; the CLI still shows nothing.
fn discover_kinds(
    runner: &dyn CommandRunner,
    kinds: &[DfKind],
    id: &str,
    label: &str,
    risk: RiskLevel,
) -> Result<Vec<CleanItem>> {
    let out = run_tool(
        runner,
        "docker",
        &["system", "df", "--format", "{{json .}}"],
    )?;
    if !out.success() {
        return Err(engine_error!(
            "docker system df failed: {}",
            out.stderr.trim()
        ));
    }
    let items = out
        .stdout
        .lines()
        .filter_map(|line| parse_df_line(line.trim()))
        .filter(|(kind, size)| kinds.contains(kind) && *size > 0)
        .map(|(kind, size)| CleanItem {
            category_id: id.to_string(),
            category_label: label.to_string(),
            path: PathBuf::from(kind.placeholder()),
            size,
            risk,
            evidence: Vec::new(),
        })
        .collect();
    Ok(items)
}

/// Runs one targeted prune per selected placeholder, so picking images
/// never touches volumes, containers or networks the way `docker system
/// prune --volumes` would. A placeholder outside `kinds` is refused.
/// Docker has no Trash semantics: Trash and HardDelete both prune; the CLI
/// warns when the user picked Trash.
fn prune(
    runner: &dyn CommandRunner,
    items: &[CleanItem],
    action: ExecAction,
    kinds: &[DfKind],
    id: &str,
) -> Result<ExecReport> {
    if matches!(action, ExecAction::EmptyTrash) {
        return Err(engine_error!("{} provider does not accept EmptyTrash", id));
    }
    let mut report = ExecReport::default();
    for item in items {
        match DfKind::from_placeholder(&item.path).filter(|k| kinds.contains(k)) {
            Some(kind) => {
                let args = kind.prune_args();
                if runner.run("docker", args).success {
                    report.removed_paths.push(item.path.clone());
                } else {
                    let reason = format!("docker {} failed", args.join(" "));
                    report.failed.push((item.path.clone(), reason));
                }
            }
            None => {
                let reason = format!("not a {} resource", id);
                report.failed.push((item.path.clone(), reason));
            }
        }
    }
    Ok(report)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DfKind {
    Images,
    BuildCache,
    Volumes,
}

impl DfKind {
    fn placeholder(self) -> &'static str {
        match self {
            DfKind::Images => PLACEHOLDER_IMAGES,
            DfKind::BuildCache => PLACEHOLDER_BUILD,
            DfKind::Volumes => PLACEHOLDER_VOLUMES,
        }
    }

    fn from_placeholder(path: &Path) -> Option<Self> {
        [DfKind::Images, DfKind::BuildCache, DfKind::Volumes]
            .into_iter()
            .find(|kind| path == Path::new(kind.placeholder()))
    }

    /// `-a` on `volume prune` (Docker 23+) also removes unused named
    /// volumes, which is what `system df` counts.
    fn prune_args(self) -> &'static [&'static str] {
        match self {
            DfKind::Images => &["image", "prune", "-af"],
            DfKind::BuildCache => &["builder", "prune", "-af"],
            DfKind::Volumes => &["volume", "prune", "-af"],
        }
    }
}

/// Parses one line of `docker system df --format '{{json .}}'`. The JSON
/// shape is `{"Type":"Images","Size":"1.234GB",...}`; we only need Type +
/// Size. Returns None for non-cleanable types (Containers).
///
/// Hand-rolled to avoid adding a serde_json dep just for this.
fn parse_df_line(line: &str) -> Option<(DfKind, u64)> {
    let kind = if line.contains("\"Type\":\"Images\"") {
        DfKind::Images
    } else if line.contains("\"Type\":\"Build Cache\"") {
        DfKind::BuildCache
    } else if line.contains("\"Type\":\"Local Volumes\"") {
        DfKind::Volumes
    } else {
        return None;
    };
    let size_str = extract_quoted(line, "Size")?;
    let size = parse_human_size(size_str)?;
    Some((kind, size))
}

/// Extracts the value of `"<key>":"<value>"` from `s`. Naive, only used
/// for trusted docker JSON output.
fn extract_quoted<'a>(s: &'a str, key: &str) -> Option<&'a str> {
    let needle = format!("\"{}\":\"", key);
    let start = s.find(&needle)? + needle.len();
    let rest = &s[start..];
    let end = rest.find('"')?;
    Some(&rest[..end])
}

/// Parses `1.23GB` / `456MB` / `0B` / `100kB` into bytes. docker uses
/// powers of 1000 for "GB"/"MB"/etc per its CLI convention.
fn parse_human_size(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    let (num_part, unit_part): (String, String) =
        s.chars().partition(|c| c.is_ascii_digit() || *c == '.');
    let num: f64 = num_part.parse().ok()?;
    let mult: f64 = match unit_part.trim() {
        "B" | "" => 1.0,
        "kB" | "KB" => 1_000.0,
        "MB" => 1_000_000.0,
        "GB" => 1_000_000_000.0,
        "TB" => 1_000_000_000_000.0,
        _ => return None,
    };
    Some((num * mult) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clean::providers::known_category_ids;
    use crate::clean::runner::test_support::MockRunner;

    #[test]
    fn docker_id_in_known_categories() {
        assert!(known_category_ids().contains(&ID));
    }

    #[test]
    fn docker_gates_desktop() {
        let p = Docker::new();
        assert_eq!(p.requires_app_quit(), Some(APP));
    }

    #[test]
    fn parse_df_line_extracts_images_and_size() {
        let line = r#"{"Type":"Images","TotalCount":"5","Active":"3","Size":"2.5GB","Reclaimable":"1.2GB"}"#;
        let (kind, size) = parse_df_line(line).unwrap();
        assert_eq!(kind, DfKind::Images);
        assert_eq!(size, 2_500_000_000);
    }

    #[test]
    fn parse_df_line_returns_none_for_containers() {
        let line = r#"{"Type":"Containers","Size":"0B"}"#;
        assert!(parse_df_line(line).is_none());
    }

    #[test]
    fn parse_human_size_handles_units() {
        assert_eq!(parse_human_size("0B"), Some(0));
        assert_eq!(parse_human_size("100B"), Some(100));
        assert_eq!(parse_human_size("1kB"), Some(1_000));
        assert_eq!(parse_human_size("2MB"), Some(2_000_000));
        assert_eq!(parse_human_size("1.5GB"), Some(1_500_000_000));
        assert_eq!(parse_human_size("garbage"), None);
    }

    #[test]
    fn docker_daemon_down_is_a_discovery_error() {
        // which("docker") succeeds (CLI installed) but `system df` fails
        // (daemon not running). Checked discovery reports it; the CLI
        // wrapper drops it, as it used to show nothing.
        let runner = Arc::new(MockRunner::new().with_which("docker").with_exit(
            "docker",
            &["system", "df", "--format", "{{json .}}"],
            1,
            "",
        ));
        let p = Docker::with_runner(runner);
        assert!(p.discover(&ScanContext::unchecked()).is_err());
    }

    #[test]
    fn docker_unavailable_when_cli_missing() {
        let runner = Arc::new(MockRunner::new());
        let p = Docker::with_runner(runner);
        assert!(!p.available());
    }

    const DF_ARGS: &[&str] = &["system", "df", "--format", "{{json .}}"];
    const DF_OUT: &str = "\
{\"Type\":\"Images\",\"Size\":\"1GB\"}
{\"Type\":\"Containers\",\"Size\":\"0B\"}
{\"Type\":\"Local Volumes\",\"Size\":\"500MB\"}
{\"Type\":\"Build Cache\",\"Size\":\"2GB\"}
";

    fn df_runner() -> MockRunner {
        MockRunner::new()
            .with_which("docker")
            .with_exit("docker", DF_ARGS, 0, DF_OUT)
    }

    /// Every command the provider ran, bounded or not.
    fn calls(runner: &MockRunner) -> Vec<String> {
        let mut calls = runner.output_calls.lock().unwrap().clone();
        calls.extend(runner.run_calls.lock().unwrap().iter().cloned());
        calls
    }

    fn ran(runner: &MockRunner, needle: &str) -> bool {
        calls(runner).iter().any(|c| c.contains(needle))
    }

    #[test]
    fn docker_discover_lists_images_and_build_cache_only() {
        let p = Docker::with_runner(Arc::new(df_runner()));
        let items = p.discover(&ScanContext::unchecked()).unwrap();
        // Containers and Local Volumes are not part of `docker`.
        let paths: Vec<_> = items.iter().map(|i| i.path.clone()).collect();
        assert_eq!(
            paths,
            vec![
                PathBuf::from(PLACEHOLDER_IMAGES),
                PathBuf::from(PLACEHOLDER_BUILD)
            ]
        );
        let total: u64 = items.iter().map(|i| i.size).sum();
        assert_eq!(total, 1_000_000_000 + 2_000_000_000);
    }

    #[test]
    fn docker_volumes_is_its_own_destructive_category() {
        let p = DockerVolumes::with_runner(Arc::new(df_runner()));
        assert_eq!(p.risk(), RiskLevel::Destructive);
        assert_eq!(p.requires_app_quit(), Some(APP));
        assert!(known_category_ids().contains(&VOLUMES_ID));
        let items = p.discover(&ScanContext::unchecked()).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].path, PathBuf::from(PLACEHOLDER_VOLUMES));
        assert_eq!(items[0].risk, RiskLevel::Destructive);
        assert_eq!(items[0].size, 500_000_000);
    }

    #[test]
    fn docker_prunes_only_the_selected_kind() {
        let runner =
            Arc::new(df_runner().with_response("docker", &["image", "prune", "-af"], true, ""));
        let p = Docker::with_runner(runner.clone());
        let images: Vec<_> = p
            .discover(&ScanContext::unchecked())
            .unwrap()
            .into_iter()
            .filter(|i| i.path == Path::new(PLACEHOLDER_IMAGES))
            .collect();
        let report = p.execute(&images, ExecAction::Trash).unwrap();
        assert_eq!(
            report.removed_paths,
            vec![PathBuf::from(PLACEHOLDER_IMAGES)]
        );
        assert!(report.failed.is_empty());
        assert_eq!(
            *runner.run_calls.lock().unwrap(),
            vec!["docker image prune -af"]
        );
        assert!(!ran(&runner, "builder prune"));
        assert!(!ran(&runner, "volume"));
        assert!(!ran(&runner, "system prune"));
    }

    #[test]
    fn docker_runs_one_prune_per_selected_kind() {
        let runner = Arc::new(
            df_runner()
                .with_response("docker", &["image", "prune", "-af"], true, "")
                .with_response("docker", &["builder", "prune", "-af"], true, ""),
        );
        let p = Docker::with_runner(runner.clone());
        let items = p.discover(&ScanContext::unchecked()).unwrap();
        let report = p.execute(&items, ExecAction::HardDelete).unwrap();
        assert_eq!(report.removed_paths.len(), 2);
        assert_eq!(
            *runner.run_calls.lock().unwrap(),
            vec!["docker image prune -af", "docker builder prune -af"]
        );
        assert!(!ran(&runner, "system prune"));
    }

    #[test]
    fn docker_refuses_a_volumes_placeholder() {
        let runner = Arc::new(MockRunner::new().with_which("docker"));
        let p = Docker::with_runner(runner.clone());
        let item = CleanItem {
            category_id: ID.to_string(),
            category_label: LABEL.to_string(),
            path: PathBuf::from(PLACEHOLDER_VOLUMES),
            size: 1,
            risk: RiskLevel::Review,
            evidence: Vec::new(),
        };
        let report = p.execute(&[item], ExecAction::HardDelete).unwrap();
        assert!(report.removed_paths.is_empty());
        assert_eq!(report.failed.len(), 1);
        assert!(calls(&runner).is_empty());
    }

    #[test]
    fn docker_volumes_refuses_an_images_placeholder_and_empty_trash() {
        let runner = Arc::new(MockRunner::new().with_which("docker"));
        let p = DockerVolumes::with_runner(runner.clone());
        let item = CleanItem {
            category_id: VOLUMES_ID.to_string(),
            category_label: VOLUMES_LABEL.to_string(),
            path: PathBuf::from(PLACEHOLDER_IMAGES),
            size: 1,
            risk: RiskLevel::Destructive,
            evidence: Vec::new(),
        };
        let report = p
            .execute(std::slice::from_ref(&item), ExecAction::HardDelete)
            .unwrap();
        assert_eq!(report.failed[0].1, "not a docker-volumes resource");
        assert!(p.execute(&[item], ExecAction::EmptyTrash).is_err());
        assert!(calls(&runner).is_empty());
    }

    #[test]
    fn docker_volumes_prune_reports_failure() {
        // `volume prune` not mocked → the runner reports failure.
        let runner = Arc::new(df_runner());
        let p = DockerVolumes::with_runner(runner.clone());
        let items = p.discover(&ScanContext::unchecked()).unwrap();
        let report = p.execute(&items, ExecAction::HardDelete).unwrap();
        assert!(report.removed_paths.is_empty());
        assert_eq!(report.failed[0].1, "docker volume prune -af failed");
        assert_eq!(
            *runner.run_calls.lock().unwrap(),
            vec!["docker volume prune -af"]
        );
        assert!(!ran(&runner, "system prune"));
    }
}
