//! A removal in progress and its outcome, one line per picked app.

/// Where one app stands.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Status {
    Waiting,
    Running,
    Removed,
    /// The engine refused or failed; its words.
    Kept(String),
    /// The run stopped before this app.
    NotRun,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RunLine {
    pub name: String,
    /// What the app's frozen plan removes under the menu's consent.
    pub bytes: u64,
    pub status: Status,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub hard: bool,
    pub lines: Vec<RunLine>,
    /// esc or quit was pressed: the run ends after the app in progress.
    pub stopping: bool,
    /// The engine refused an app before the menu opened: nothing ran.
    pub refused: bool,
}

impl Run {
    /// A run about to start over `apps` (name, removable bytes).
    pub fn new(apps: Vec<(String, u64)>, hard: bool) -> Self {
        Self {
            hard,
            lines: apps
                .into_iter()
                .map(|(name, bytes)| RunLine {
                    name,
                    bytes,
                    status: Status::Waiting,
                })
                .collect(),
            stopping: false,
            refused: false,
        }
    }

    /// Freezing found refusals: those apps keep the engine's words, the
    /// others did not run.
    pub fn refused(apps: Vec<(String, u64, Option<String>)>) -> Self {
        Self {
            hard: false,
            lines: apps
                .into_iter()
                .map(|(name, bytes, refusal)| RunLine {
                    name,
                    bytes,
                    status: refusal.map_or(Status::NotRun, Status::Kept),
                })
                .collect(),
            stopping: false,
            refused: true,
        }
    }

    pub fn start(&mut self, index: usize) {
        self.set(index, Status::Running);
    }

    pub fn finish(&mut self, index: usize, result: Result<(), String>) {
        self.set(
            index,
            result.map_or_else(Status::Kept, |()| Status::Removed),
        );
    }

    /// The run is over: an app still running when its task ended is reported
    /// with `reason`; apps that never started did not run.
    pub fn end(self, reason: Option<&str>) -> Self {
        let lines = self
            .lines
            .into_iter()
            .map(|line| {
                let status = match line.status {
                    Status::Waiting => Status::NotRun,
                    Status::Running => {
                        Status::Kept(reason.unwrap_or("the run ended without a result").into())
                    }
                    done => done,
                };
                RunLine { status, ..line }
            })
            .collect();
        Self { lines, ..self }
    }

    pub fn removed_bytes(&self) -> u64 {
        self.lines
            .iter()
            .filter(|line| line.status == Status::Removed)
            .map(|line| line.bytes)
            .fold(0u64, u64::saturating_add)
    }

    pub fn count(&self, matches: impl Fn(&Status) -> bool) -> usize {
        self.lines
            .iter()
            .filter(|line| matches(&line.status))
            .count()
    }

    pub fn first_kept(&self) -> Option<(&str, &str)> {
        self.lines.iter().find_map(|line| match &line.status {
            Status::Kept(reason) => Some((line.name.as_str(), reason.as_str())),
            _ => None,
        })
    }

    fn set(&mut self, index: usize, status: Status) {
        if let Some(line) = self.lines.get_mut(index) {
            line.status = status;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_run_reports_each_app_and_what_never_started() {
        let mut run = Run::new(
            vec![("A".into(), 10), ("B".into(), 20), ("C".into(), 30)],
            false,
        );
        run.start(0);
        run.finish(0, Ok(()));
        run.start(1);
        run.finish(1, Err("B is running".into()));
        let run = run.end(None);
        let statuses: Vec<_> = run.lines.iter().map(|l| l.status.clone()).collect();
        assert_eq!(
            statuses,
            [
                Status::Removed,
                Status::Kept("B is running".into()),
                Status::NotRun
            ]
        );
        assert_eq!(run.removed_bytes(), 10);
        assert_eq!(run.first_kept(), Some(("B", "B is running")));
    }

    #[test]
    fn an_app_cut_off_by_a_failed_task_keeps_the_reason() {
        let mut run = Run::new(vec![("A".into(), 10)], true);
        run.start(0);
        let run = run.end(Some("stopped unexpectedly: boom"));
        assert_eq!(
            run.lines[0].status,
            Status::Kept("stopped unexpectedly: boom".into())
        );
    }
}
