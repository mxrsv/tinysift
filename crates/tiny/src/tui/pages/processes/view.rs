//! Processes' column heading, rows, detail, confirmation boxes and outcome
//! lines, drawn as the mock's `procs` and `quit` screens.

use tiny_core::processes::{
    ParentState, ProcessInfo, Refusal, TerminateKind, TerminateOutcome, TerminateTarget,
};

use super::engine::{Ports, ProcessRow};
use crate::tui::pages::Detail;
use crate::tui::styled_line::{Role, StyledLine};
use crate::tui::widgets::confirm_box::{ConfirmBox, Tone};
use crate::tui::widgets::meter::{meter, name_and_bar, BAR};
use crate::tui::widgets::size_format::format_size;
use crate::tui::widgets::text_fit::{cut, wrap};

/// Cells before the name: lead (2), PID (6), gap (2), bar (12), gap (1),
/// CPU% (7), gap (1), memory (9), gap (2).
const BEFORE_NAME: usize = 42;
/// The CPU bar turns to the warning role above this percent, and to the bad role above the next.
const CPU_WARN_ABOVE: f32 = 50.0;
const CPU_BAD_ABOVE: f32 = 90.0;
/// Detail labels (`Parent      `) are this wide.
const LABEL: usize = 12;

pub const CPU_NOTE: &str = "CPU is per core, so one process can pass 100%.";

/// The name and bar widths of a row `list_width` cells wide.
fn columns(list_width: usize) -> (usize, usize) {
    name_and_bar(list_width.saturating_sub(BEFORE_NAME - BAR))
}

pub fn heading(list_width: usize) -> StyledLine {
    let (_, bar) = columns(list_width);
    StyledLine::new().dim(format!(
        "  {:>6}  {:<cpu$}{:>10}  NAME",
        "PID",
        "CPU",
        "MEMORY",
        cpu = bar + 8
    ))
}

pub fn row(info: &ProcessInfo, highlighted: bool, list_width: usize) -> StyledLine {
    let value = if highlighted { Role::Hi } else { Role::Plain };
    let lead = if highlighted {
        StyledLine::new().hi("❯ ")
    } else {
        StyledLine::from("  ")
    };
    let cpu = info.cpu_percent;
    let bar_role = match cpu {
        Some(cpu) if cpu > CPU_BAD_ABOVE => Role::Bad,
        Some(cpu) if cpu > CPU_WARN_ABOVE => Role::Warn,
        _ => Role::Cpu,
    };
    let fraction = cpu.map_or(0.0, |cpu| f64::from(cpu / 100.0));
    let (name, bar) = columns(list_width);
    lead.dim(format!("{:>6}", info.pid))
        .plain("  ")
        .append(meter(fraction, bar, bar_role))
        .plain(" ")
        .push(value, format!("{:>7}", cpu_cell(cpu)))
        .plain(format!(" {:>9}  ", memory_cell(info.memory_bytes)))
        .push(value, cut(&info.name, name))
}

pub fn detail(row: &ProcessRow, refused: Option<&Refusal>, width: usize) -> Detail {
    let info = &row.info;
    let who = format!("PID {} · {}", info.pid, owner(info));
    let parent = parent_cell(&row.parent);
    let memory = memory_cell(info.memory_bytes);
    let ports = ports_cell(&row.ports);
    let refusal = refused.map(|reason| format!("Cannot quit: {reason}"));
    let labelled = |label: &str, value: &str| {
        StyledLine::new()
            .dim(format!("{label:<LABEL$}"))
            .plain(cut(value, width.saturating_sub(LABEL)))
    };
    let full = wrap(&info.name, width)
        .into_iter()
        .map(|line| StyledLine::new().hi(line))
        .chain([
            StyledLine::new().dim(who.as_str()),
            StyledLine::new(),
            labelled("Parent", &parent),
            labelled("Children", &row.children.to_string()),
            labelled("Memory", &memory),
        ])
        .chain(
            wrap(&ports, width.saturating_sub(LABEL))
                .into_iter()
                .enumerate()
                .map(|(i, line)| {
                    let label = if i == 0 { "Listens on" } else { "" };
                    StyledLine::new()
                        .dim(format!("{label:<LABEL$}"))
                        .plain(line)
                }),
        )
        .chain(refusal.iter().flat_map(|text| {
            std::iter::once(StyledLine::new()).chain(
                wrap(text, width.saturating_sub(2))
                    .into_iter()
                    .enumerate()
                    .map(|(i, line)| {
                        let lead = if i == 0 {
                            StyledLine::new().warn("● ")
                        } else {
                            StyledLine::from("  ")
                        };
                        lead.plain(line)
                    }),
            )
        }))
        .collect();
    let last = match &refusal {
        Some(text) => StyledLine::new().warn("● ").plain(text.as_str()),
        None => StyledLine::new().dim("Listens on  ").plain(ports),
    };
    let brief = vec![
        StyledLine::new()
            .hi(cut(&info.name, width.saturating_sub(22)))
            .dim(format!("  {who}")),
        StyledLine::new()
            .dim("Parent ")
            .plain(cut(&parent, 28))
            .dim("   children ")
            .plain(row.children.to_string())
            .dim("   memory ")
            .plain(memory),
        last,
    ];
    Detail {
        title: "Highlighted".into(),
        full,
        brief,
    }
}

/// The box `x` (Quit) or `X` (Force quit) opens, Cancel focused.
pub fn confirm_box(info: &ProcessInfo, kind: TerminateKind) -> ConfirmBox {
    match kind {
        TerminateKind::Graceful => ConfirmBox::new(
            "Quit process",
            Tone::Warning,
            format!("Quit {}?", info.name),
            "Quit",
        )
        .with_facts(vec![
            StyledLine::new().dim(format!("PID {} is asked to close.", info.pid)),
            StyledLine::new().dim("Unsaved work may be lost."),
        ]),
        TerminateKind::Force => ConfirmBox::new(
            "Force quit process",
            Tone::Permanent,
            format!("Force quit {}?", info.name),
            "Force quit",
        )
        .with_facts(vec![
            StyledLine::new().dim(format!(
                "PID {} ends at once, with no chance to save.",
                info.pid
            )),
            StyledLine::new().bad("Unsaved work is lost."),
        ]),
    }
}

/// The engine's answer to a confirmed quit, for the foot.
pub fn outcome_line(
    target: &TerminateTarget,
    kind: TerminateKind,
    outcome: &Result<TerminateOutcome, String>,
) -> StyledLine {
    let who = format!("{} (PID {})", target.name, target.pid);
    let bad = |text: String| StyledLine::new().bad("● ").plain(text);
    let warn = |text: String| StyledLine::new().warn("● ").plain(text);
    match outcome {
        Ok(TerminateOutcome::Exited) => StyledLine::new().ok("✓ ").plain(format!("{who} ended.")),
        Ok(TerminateOutcome::StillRunning) => match kind {
            TerminateKind::Graceful => warn(format!("{who} is still running. X force quits it.")),
            TerminateKind::Force => bad(format!("{who} is still running after force quit.")),
        },
        Ok(TerminateOutcome::AlreadyExited) => {
            warn(format!("{who} had already ended. Nothing was sent."))
        }
        Ok(TerminateOutcome::PermissionDenied) => bad(format!("macOS denied the signal to {who}.")),
        Ok(TerminateOutcome::IdentityChanged) => warn(format!(
            "PID {} now belongs to another process. Nothing was sent.",
            target.pid
        )),
        Ok(TerminateOutcome::Refused(reason)) => {
            warn(format!("Cannot quit {who}: {reason}. Nothing was sent."))
        }
        Err(error) => bad(format!("Quitting {who} failed: {error}")),
    }
}

fn owner(info: &ProcessInfo) -> String {
    if info.is_current_user {
        "yours".into()
    } else {
        info.user.clone().unwrap_or_else(|| "another user".into())
    }
}

fn parent_cell(parent: &ParentState) -> String {
    match parent {
        ParentState::None => "none".into(),
        ParentState::Running { pid, name } => format!("{name} ({pid})"),
        ParentState::Exited { pid } => format!("PID {pid}, ended"),
    }
}

fn ports_cell(ports: &Ports) -> String {
    match ports {
        Ports::Listening(ports) if ports.is_empty() => "no port".into(),
        Ports::Listening(ports) => ports
            .iter()
            .map(|port| format!(":{port}"))
            .collect::<Vec<_>>()
            .join(" "),
        Ports::NotVisible => "not visible for another user's process".into(),
        Ports::Unavailable => "unavailable".into(),
    }
}

fn cpu_cell(cpu: Option<f32>) -> String {
    cpu.map_or_else(|| "n/a".into(), |cpu| format!("{cpu:.1}%"))
}

fn memory_cell(bytes: Option<u64>) -> String {
    bytes.map_or_else(|| "n/a".into(), format_size)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tui::styled_line::text_width;

    #[test]
    fn the_heading_and_a_row_line_up_like_the_mock() {
        assert_eq!(
            heading(78).text(),
            "     PID  CPU                     MEMORY  NAME"
        );
        let info = ProcessInfo {
            pid: 43154,
            name: "AltTab".into(),
            user: None,
            uid: None,
            is_current_user: true,
            parent_pid: Some(1),
            start_time: 0,
            cpu_percent: Some(19.6),
            cpu_measured: true,
            memory_bytes: Some(89_000_000),
            footprint_bytes: None,
            cpu_time_ns: None,
            responsible: None,
            exe: None,
        };
        let line = row(&info, false, 78);
        assert_eq!(
            line.text(),
            "   43154  ━━━━━━━━━━━━   19.6%     89 MB  AltTab"
        );
        assert_eq!(text_width(&line.text()), BEFORE_NAME + "AltTab".len());
        // At 60 columns the bar gives way so the name keeps its room.
        assert_eq!(heading(58).text(), "     PID  CPU             MEMORY  NAME");
        assert_eq!(
            row(&info, false, 58).text(),
            "   43154  ━━━━   19.6%     89 MB  AltTab"
        );
    }

    #[test]
    fn force_quit_names_the_loss_in_the_bad_role() {
        let info = ProcessInfo {
            pid: 7,
            name: "x".into(),
            user: None,
            uid: None,
            is_current_user: true,
            parent_pid: None,
            start_time: 0,
            cpu_percent: None,
            cpu_measured: false,
            memory_bytes: None,
            footprint_bytes: None,
            cpu_time_ns: None,
            responsible: None,
            exe: None,
        };
        let lines = confirm_box(&info, TerminateKind::Force).overlay().lines;
        let loss = lines
            .iter()
            .find(|line| line.text().contains("Unsaved work is lost."))
            .expect("the consequence line");
        assert!(loss
            .segs()
            .iter()
            .any(|seg| seg.role == Role::Bad && seg.text == "Unsaved work is lost."));
        assert_eq!(lines[1].segs()[0].role, Role::Bad, "the dot is red");
    }
}
