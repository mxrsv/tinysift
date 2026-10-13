//! `tiny processes`: list, show and quit, backed by the same
//! `tiny_core::processes` calls as the native app.

use anyhow::{bail, Context, Result};
use dialoguer::{theme::ColorfulTheme, Confirm};
use serde_json::json;
use tiny_core::processes::{
    self, ParentState, PortOwners, ProcessDetail, ProcessInfo, Sampler, TerminateKind,
    TerminateOutcome, TerminateTarget,
};
use tiny_core::runner::RealRunner;

use crate::cli::{ProcessesAction, ProcessesOpts};
use crate::util::format_bytes;

/// Like `TINY_CONFIRM_HARD` for `clean --hard --yes`.
const CONFIRM_FORCE_ENV: &str = "TINY_CONFIRM_FORCE";

pub fn run(opts: ProcessesOpts) -> Result<()> {
    match &opts.action {
        Some(ProcessesAction::Show { pid, json }) => show(*pid, *json),
        Some(ProcessesAction::Quit { pid, force, yes }) => quit(*pid, *force, *yes),
        None => match opts.port {
            Some(port) => list_port(port, &opts),
            None => list(&opts),
        },
    }
}

fn list(opts: &ProcessesOpts) -> Result<()> {
    let mut snapshot = Sampler::new().sample_measured();
    processes::sort_processes(&mut snapshot.processes, opts.sort.into());
    snapshot.processes.truncate(opts.limit);
    if opts.json {
        println!("{}", serde_json::to_string_pretty(&snapshot)?);
        return Ok(());
    }
    print_sample_line(snapshot.sampled_at);
    print_table(&snapshot.processes);
    Ok(())
}

/// Lists every visible owner (no `--limit`), keeping the list's JSON shape.
fn list_port(port: u16, opts: &ProcessesOpts) -> Result<()> {
    let mut sampler = Sampler::new();
    let found = processes::port_owners(port, &RealRunner, || sampler.sample_measured())?;
    let mut listed: Vec<ProcessInfo> = found
        .owners
        .iter()
        .filter_map(|owner| owner.process.clone())
        .collect();
    processes::sort_processes(&mut listed, opts.sort.into());
    if opts.json {
        let report = port_report_json(&found, &listed);
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("Visible owners of TCP port {port}");
    print_sample_line(found.sampled_at);
    if found.owners.is_empty() {
        println!("No visible owner.");
    } else {
        print_table(&listed);
    }
    for owner in &found.owners {
        match (&owner.process, &owner.refusal) {
            (None, _) => println!("PID {}: not in the sample, cannot be quit", owner.pid),
            (Some(_), Some(refusal)) => println!("PID {}: cannot be quit, {refusal}", owner.pid),
            (Some(_), None) => {}
        }
    }
    println!("Note: {}", found.visibility_caveat);
    Ok(())
}

/// The list's JSON shape plus every owner's actionability and the caveat.
fn port_report_json(found: &PortOwners, listed: &[ProcessInfo]) -> serde_json::Value {
    let owners: Vec<_> = found
        .owners
        .iter()
        .map(|owner| {
            json!({
                "pid": owner.pid,
                "inSample": owner.process.is_some(),
                "actionable": owner.actionable(),
                "refusal": owner.refusal,
            })
        })
        .collect();
    json!({
        "processes": listed,
        "sampledAt": found.sampled_at,
        "cpuMeasured": found.cpu_measured,
        "port": found.port,
        "portOwners": owners,
        "visibilityCaveat": found.visibility_caveat,
    })
}

fn print_sample_line(sampled_at: u64) {
    println!(
        "Sampled at {sampled_at} (Unix seconds). CPU is per core; n/a = not measured or not permitted."
    );
}

fn print_table(processes: &[ProcessInfo]) {
    println!(
        "{:>7}  {:>6}  {:>10}  {:<12}  NAME",
        "PID", "CPU%", "MEMORY", "USER"
    );
    for process in processes {
        println!(
            "{:>7}  {:>6}  {:>10}  {:<12}  {}",
            process.pid,
            cpu_cell(process),
            memory_cell(process),
            process.user.as_deref().unwrap_or("?"),
            process.name
        );
    }
}

fn cpu_cell(process: &ProcessInfo) -> String {
    process
        .cpu_percent
        .map_or_else(|| "n/a".into(), |cpu| format!("{cpu:.1}"))
}

fn memory_cell(process: &ProcessInfo) -> String {
    process
        .memory_bytes
        .map_or_else(|| "n/a".into(), format_bytes)
}

fn show(pid: u32, as_json: bool) -> Result<()> {
    let snapshot = Sampler::new().sample_measured();
    let detail = processes::detail(&snapshot, pid, &RealRunner)?;
    if as_json {
        let mut report = serde_json::to_value(&detail)?;
        report["sampledAt"] = json!(snapshot.sampled_at);
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    print_detail(&detail);
    print_sample_line(snapshot.sampled_at);
    Ok(())
}

fn print_detail(detail: &ProcessDetail) {
    let process = &detail.process;
    println!("{} (PID {})", process.name, process.pid);
    println!("User:       {}", process.user.as_deref().unwrap_or("?"));
    println!("Started:    {} (Unix seconds)", process.start_time);
    println!("CPU:        {}", cpu_cell(process));
    println!("Memory:     {}", memory_cell(process));
    if let Some(exe) = &process.exe {
        println!("Executable: {}", exe.display());
    }
    match &detail.parent {
        ParentState::None => println!("Parent:     none"),
        ParentState::Running { pid, name } => println!("Parent:     {name} (PID {pid})"),
        ParentState::Exited { pid } => println!("Parent:     PID {pid} (exited)"),
    }
    println!("Children:   {}", detail.children.len());
    for child in &detail.children {
        println!("  {:>7}  {}", child.pid, child.name);
    }
    match (&detail.ports, &detail.ports_error) {
        (Some(ports), _) if ports.is_empty() => println!("Ports:      none listening"),
        (Some(ports), _) => {
            for port in ports {
                println!("Port:       {}:{}", port.address, port.port);
            }
        }
        (None, error) => println!(
            "Ports:      unavailable ({})",
            error.as_deref().unwrap_or("unknown")
        ),
    }
}

fn quit(pid: u32, force: bool, yes: bool) -> Result<()> {
    if force && yes && std::env::var(CONFIRM_FORCE_ENV).as_deref() != Ok("1") {
        bail!("--force with --yes requires {CONFIRM_FORCE_ENV}=1 in the environment");
    }
    let snapshot = Sampler::new().sample();
    let info = snapshot
        .processes
        .iter()
        .find(|p| p.pid == pid)
        .with_context(|| format!("process {pid} is not running"))?;
    if let Some(refusal) = processes::refusal(info) {
        bail!("refusing to quit {} (PID {pid}): {refusal}", info.name);
    }
    let kind = if force {
        TerminateKind::Force
    } else {
        TerminateKind::Graceful
    };
    if !yes && !confirm_quit(info, kind)? {
        println!("Cancelled; no signal was sent.");
        return Ok(());
    }
    let outcome = processes::terminate(&TerminateTarget::from(info), kind)?;
    report_outcome(info, kind, outcome)
}

fn confirm_quit(info: &ProcessInfo, kind: TerminateKind) -> Result<bool> {
    let prompt = match kind {
        TerminateKind::Graceful => format!(
            "Quit {} (PID {})? It is asked to exit with SIGTERM; unsaved work may be lost.",
            info.name, info.pid
        ),
        TerminateKind::Force => format!(
            "FORCE QUIT {} (PID {})? SIGKILL ends it immediately; unsaved work WILL be lost.",
            info.name, info.pid
        ),
    };
    Confirm::with_theme(&ColorfulTheme::default())
        .with_prompt(prompt)
        .default(false)
        .interact()
        .context("quit confirm prompt failed")
}

/// Only `Exited` succeeds; every other outcome exits non-zero with its reason.
fn report_outcome(
    info: &ProcessInfo,
    kind: TerminateKind,
    outcome: TerminateOutcome,
) -> Result<()> {
    let target = format!("{} (PID {})", info.name, info.pid);
    let signal = match kind {
        TerminateKind::Graceful => "SIGTERM",
        TerminateKind::Force => "SIGKILL",
    };
    match outcome {
        TerminateOutcome::Exited => {
            println!("{target} exited after {signal}.");
            Ok(())
        }
        TerminateOutcome::StillRunning => match kind {
            TerminateKind::Graceful => {
                bail!("{target} is still running after {signal}; use --force to send SIGKILL")
            }
            TerminateKind::Force => bail!("{target} is still running after {signal}"),
        },
        TerminateOutcome::AlreadyExited => {
            bail!("{target} had already exited; nothing was signalled")
        }
        TerminateOutcome::PermissionDenied => {
            bail!("permission denied sending {signal} to {target}")
        }
        TerminateOutcome::IdentityChanged => bail!(
            "PID {} now belongs to a different process; nothing was signalled",
            info.pid
        ),
        TerminateOutcome::Refused(refusal) => bail!("refusing to quit {target}: {refusal}"),
    }
}
