//! Process queries and actions exported to Swift. Mirror records keep
//! `tiny-core` types off the boundary.

use std::path::PathBuf;

use tiny_core::processes::{
    self, Listener, Listeners, ListeningPort, LoadAverage, MemoryPressure, NetworkThroughput,
    ParentState, PortOwner, PortOwners, ProcessInfo, ProcessSnapshot, Refusal, SystemUsage,
    TerminateKind, TerminateOutcome, TerminateTarget,
};
use tiny_core::runner::RealRunner;

use crate::{FfiError, TinySession};

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiProcessInfo {
    pub pid: u32,
    pub name: String,
    pub user: Option<String>,
    pub is_current_user: bool,
    pub parent_pid: Option<u32>,
    pub start_time: u64,
    /// Per-core percentage; can exceed 100. `None` when not measured.
    pub cpu_percent: Option<f32>,
    pub cpu_measured: bool,
    /// Physical footprint, not resident memory: dirty private memory plus its compressed
    /// and swapped pages. `None` when it cannot be read.
    pub memory_bytes: Option<u64>,
    pub executable_path: Option<String>,
    /// User plus system CPU time since the process started, including the children it has
    /// reaped, in nanoseconds; it only grows while the process lives. `None` exactly when
    /// `memory_bytes` is: another user's process, or one that is gone. Never zero for unread.
    #[uniffi(default = None)]
    pub cpu_time_ns: Option<u64>,
    /// The process macOS holds responsible for this one (the app an XPC service, helper or
    /// decoder works for), as its PID plus start time. Both are `None` when macOS gives no
    /// answer (the lookup is private API), the responsible process is not in the same sample,
    /// or it is this process; the app then groups by the parent chain.
    #[uniffi(default = None)]
    pub responsible_pid: Option<u32>,
    #[uniffi(default = None)]
    pub responsible_start_time: Option<u64>,
    /// Why `process_terminate` would refuse this process, if anything. Lets
    /// the UI disable Quit up front instead of offering a refused action.
    pub refusal: Option<FfiRefusal>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiProcessSnapshot {
    pub processes: Vec<FfiProcessInfo>,
    pub sampled_at: u64,
    pub cpu_measured: bool,
    pub system_usage: FfiSystemUsage,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiSystemUsage {
    pub cpu_percent: Option<f32>,
    pub cpu_warming_up: bool,
    pub memory_used_bytes: Option<u64>,
    pub memory_total_bytes: Option<u64>,
    /// `memory_used_bytes` split into app, wired and compressed memory. All three are
    /// `Some` or all `None`, and sum to the used figure.
    pub memory_apps_bytes: Option<u64>,
    pub memory_wired_bytes: Option<u64>,
    pub memory_compressed_bytes: Option<u64>,
    /// Per-core 0–100; an entry is `None` when that core did not report. Empty while warming up.
    pub cpu_cores: Vec<Option<f32>>,
    pub cpu_brand: Option<String>,
    pub logical_cores: Option<u32>,
    pub load_average: Option<FfiLoadAverage>,
    /// Both `Some(0)` means swap is not in use.
    pub swap_used_bytes: Option<u64>,
    pub swap_total_bytes: Option<u64>,
    pub memory_pressure: Option<FfiMemoryPressure>,
    /// Whole-machine bytes per second since the previous sample, over the Mac's ports and
    /// AirDrop links. `None` while the first sample sets the baseline (`network_warming_up`)
    /// or when no interface could be read.
    #[uniffi(default = None)]
    pub network: Option<FfiNetworkThroughput>,
    #[uniffi(default = false)]
    pub network_warming_up: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct FfiNetworkThroughput {
    pub received_bytes_per_second: u64,
    pub sent_bytes_per_second: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, uniffi::Record)]
pub struct FfiLoadAverage {
    pub one: f64,
    pub five: f64,
    pub fifteen: f64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiMemoryPressure {
    Normal,
    Warning,
    Critical,
}

#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum FfiParentState {
    None,
    Running { pid: u32, name: String },
    Exited { pid: u32 },
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiListeningPort {
    pub address: String,
    pub port: u16,
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiProcessDetail {
    pub process: FfiProcessInfo,
    pub parent: FfiParentState,
    pub children: Vec<FfiProcessInfo>,
    pub ports: Option<Vec<FfiListeningPort>>,
    pub ports_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum FfiTerminateKind {
    /// SIGTERM.
    Graceful,
    /// SIGKILL; confirmed separately in the UI.
    Force,
}

/// The process as the user saw it when confirming. Copy these fields from the
/// `FfiProcessInfo` shown; `executable_path` is compared only when present.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiTerminateTarget {
    pub pid: u32,
    pub start_time: u64,
    pub name: String,
    pub executable_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum FfiRefusal {
    OwnProcess,
    OwnParent,
    SystemProcess,
    OtherUser,
    Protected {
        name: String,
    },
    /// Port lookups only: started after the lookup, so the PID may be reused.
    IdentityUncertain,
}

#[derive(Debug, Clone, PartialEq, uniffi::Enum)]
pub enum FfiTerminateOutcome {
    Exited,
    StillRunning,
    AlreadyExited,
    PermissionDenied,
    IdentityChanged,
    Refused { reason: FfiRefusal },
}

#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiPortOwner {
    pub pid: u32,
    /// `None` when the PID was not in the sample taken after the probe.
    pub process: Option<FfiProcessInfo>,
    pub refusal: Option<FfiRefusal>,
    /// In the sample and not refused, so Quit/Force Quit may be offered.
    pub actionable: bool,
}

/// An empty `owners` list never means the port is free; show `visibility_caveat`.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiPortOwners {
    pub port: u16,
    pub owners: Vec<FfiPortOwner>,
    pub visibility_caveat: String,
    pub sampled_at: u64,
}

/// One visible listening socket and its owner.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiListener {
    pub port: u16,
    pub address: String,
    pub pid: u32,
    /// `None` when the PID was not in the sample taken after the probe.
    pub process: Option<FfiProcessInfo>,
    pub refusal: Option<FfiRefusal>,
    pub actionable: bool,
}

/// Never complete: other users' listeners are invisible without root, so
/// always show `visibility_caveat` with the list.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiListeners {
    /// Sorted by port, then PID, then address.
    pub listeners: Vec<FfiListener>,
    pub visibility_caveat: String,
    pub sampled_at: u64,
}

/// Where one process runs from. `path` is `None` when macOS denies the read or the process is gone.
#[derive(Debug, Clone, PartialEq, uniffi::Record)]
pub struct FfiWorkingDirectory {
    pub pid: u32,
    pub path: Option<String>,
}

#[uniffi::export]
impl TinySession {
    /// Working directories of the given PIDs, in order, for naming agent sessions by folder. Read-only
    /// and cheap (one syscall per PID), so the caller asks only for the few roots it needs. Bypasses the gate.
    pub fn process_working_directories(&self, pids: Vec<u32>) -> Vec<FfiWorkingDirectory> {
        pids.into_iter()
            .map(|pid| FfiWorkingDirectory {
                pid,
                path: processes::working_directory(pid),
            })
            .collect()
    }

    /// Samples all processes. CPU is measured from a process's third sample.
    pub fn process_list(&self) -> Result<FfiProcessSnapshot, FfiError> {
        Ok(self.sampler().sample().into())
    }

    /// Whole-machine CPU, memory, swap, pressure and network without walking processes, for
    /// the menu bar's background record. CPU and network read warming up on the first call
    /// and need calls at least 0.2 s apart. Bypasses the gate.
    pub fn machine_usage(&self) -> FfiSystemUsage {
        self.machine().sample().into()
    }

    /// Resamples, then returns detail, parent/children and listening ports.
    pub fn process_detail(&self, pid: u32) -> Result<FfiProcessDetail, FfiError> {
        let snapshot = self.sampler().sample();
        let detail = processes::detail(&snapshot, pid, &RealRunner)?;
        Ok(FfiProcessDetail {
            process: detail.process.into(),
            parent: detail.parent.into(),
            children: detail.children.into_iter().map(Into::into).collect(),
            ports: detail
                .ports
                .map(|ports| ports.into_iter().map(Into::into).collect()),
            ports_error: detail.ports_error,
        })
    }

    /// Revalidates the target, then sends one signal. Bypasses the gate, so
    /// Quit of a runaway process does not wait minutes for a scan to finish;
    /// the app sends one action at a time.
    pub fn process_terminate(
        &self,
        target: FfiTerminateTarget,
        kind: FfiTerminateKind,
    ) -> Result<FfiTerminateOutcome, FfiError> {
        Ok(processes::terminate(&target.into(), kind.into())?.into())
    }

    /// Read-only: visible listeners on a TCP port. Bypasses the gate.
    pub fn process_port_owners(&self, port: u16) -> Result<FfiPortOwners, FfiError> {
        let owners = processes::port_owners(port, &RealRunner, || self.sampler().sample())?;
        Ok(owners.into())
    }

    /// Read-only: every visible listening TCP socket. Bypasses the gate.
    pub fn process_listeners(&self) -> Result<FfiListeners, FfiError> {
        let found = processes::listeners(&RealRunner, || self.sampler().sample())?;
        Ok(found.into())
    }
}

impl From<Listener> for FfiListener {
    fn from(listener: Listener) -> Self {
        Self {
            port: listener.port,
            address: listener.address,
            pid: listener.pid,
            process: listener.process.map(Into::into),
            refusal: listener.refusal.map(Into::into),
            actionable: listener.actionable,
        }
    }
}

impl From<Listeners> for FfiListeners {
    fn from(found: Listeners) -> Self {
        Self {
            listeners: found.listeners.into_iter().map(Into::into).collect(),
            visibility_caveat: found.visibility_caveat,
            sampled_at: found.sampled_at,
        }
    }
}

impl From<FfiTerminateTarget> for TerminateTarget {
    fn from(target: FfiTerminateTarget) -> Self {
        Self {
            pid: target.pid,
            start_time: target.start_time,
            name: target.name,
            exe: target.executable_path.map(PathBuf::from),
        }
    }
}

impl From<FfiTerminateKind> for TerminateKind {
    fn from(kind: FfiTerminateKind) -> Self {
        match kind {
            FfiTerminateKind::Graceful => Self::Graceful,
            FfiTerminateKind::Force => Self::Force,
        }
    }
}

impl From<Refusal> for FfiRefusal {
    fn from(refusal: Refusal) -> Self {
        match refusal {
            Refusal::OwnProcess => Self::OwnProcess,
            Refusal::OwnParent => Self::OwnParent,
            Refusal::SystemProcess => Self::SystemProcess,
            Refusal::OtherUser => Self::OtherUser,
            Refusal::Protected { name } => Self::Protected { name },
            Refusal::IdentityUncertain => Self::IdentityUncertain,
        }
    }
}

impl From<TerminateOutcome> for FfiTerminateOutcome {
    fn from(outcome: TerminateOutcome) -> Self {
        match outcome {
            TerminateOutcome::Exited => Self::Exited,
            TerminateOutcome::StillRunning => Self::StillRunning,
            TerminateOutcome::AlreadyExited => Self::AlreadyExited,
            TerminateOutcome::PermissionDenied => Self::PermissionDenied,
            TerminateOutcome::IdentityChanged => Self::IdentityChanged,
            TerminateOutcome::Refused(reason) => Self::Refused {
                reason: reason.into(),
            },
        }
    }
}

impl From<PortOwner> for FfiPortOwner {
    fn from(owner: PortOwner) -> Self {
        let actionable = owner.actionable();
        Self {
            pid: owner.pid,
            process: owner.process.map(Into::into),
            refusal: owner.refusal.map(Into::into),
            actionable,
        }
    }
}

impl From<PortOwners> for FfiPortOwners {
    fn from(found: PortOwners) -> Self {
        Self {
            port: found.port,
            owners: found.owners.into_iter().map(Into::into).collect(),
            visibility_caveat: found.visibility_caveat,
            sampled_at: found.sampled_at,
        }
    }
}

impl From<ProcessInfo> for FfiProcessInfo {
    fn from(info: ProcessInfo) -> Self {
        let refusal = processes::refusal(&info).map(Into::into);
        Self {
            pid: info.pid,
            name: info.name,
            user: info.user,
            is_current_user: info.is_current_user,
            parent_pid: info.parent_pid,
            start_time: info.start_time,
            cpu_percent: info.cpu_percent,
            cpu_measured: info.cpu_measured,
            memory_bytes: info.footprint_bytes,
            executable_path: info
                .exe
                .and_then(|path| path.into_os_string().into_string().ok()),
            cpu_time_ns: info.cpu_time_ns,
            responsible_pid: info.responsible_pid,
            responsible_start_time: info.responsible_start_time,
            refusal,
        }
    }
}

impl From<ProcessSnapshot> for FfiProcessSnapshot {
    fn from(snapshot: ProcessSnapshot) -> Self {
        Self {
            processes: snapshot.processes.into_iter().map(Into::into).collect(),
            sampled_at: snapshot.sampled_at,
            cpu_measured: snapshot.cpu_measured,
            system_usage: snapshot.system_usage.into(),
        }
    }
}

impl From<SystemUsage> for FfiSystemUsage {
    fn from(usage: SystemUsage) -> Self {
        Self {
            cpu_percent: usage.cpu_percent,
            cpu_warming_up: usage.cpu_warming_up,
            memory_used_bytes: usage.memory_used_bytes,
            memory_total_bytes: usage.memory_total_bytes,
            memory_apps_bytes: usage.memory_apps_bytes,
            memory_wired_bytes: usage.memory_wired_bytes,
            memory_compressed_bytes: usage.memory_compressed_bytes,
            cpu_cores: usage.cpu_cores,
            cpu_brand: usage.cpu_brand,
            logical_cores: usage.logical_cores,
            load_average: usage.load_average.map(Into::into),
            swap_used_bytes: usage.swap_used_bytes,
            swap_total_bytes: usage.swap_total_bytes,
            memory_pressure: usage.memory_pressure.map(Into::into),
            network: usage.network.map(Into::into),
            network_warming_up: usage.network_warming_up,
        }
    }
}

impl From<NetworkThroughput> for FfiNetworkThroughput {
    fn from(rate: NetworkThroughput) -> Self {
        Self {
            received_bytes_per_second: rate.received_bytes_per_second,
            sent_bytes_per_second: rate.sent_bytes_per_second,
        }
    }
}

impl From<LoadAverage> for FfiLoadAverage {
    fn from(load: LoadAverage) -> Self {
        Self {
            one: load.one,
            five: load.five,
            fifteen: load.fifteen,
        }
    }
}

impl From<MemoryPressure> for FfiMemoryPressure {
    fn from(pressure: MemoryPressure) -> Self {
        match pressure {
            MemoryPressure::Normal => Self::Normal,
            MemoryPressure::Warning => Self::Warning,
            MemoryPressure::Critical => Self::Critical,
        }
    }
}

impl From<ParentState> for FfiParentState {
    fn from(parent: ParentState) -> Self {
        match parent {
            ParentState::None => Self::None,
            ParentState::Running { pid, name } => Self::Running { pid, name },
            ParentState::Exited { pid } => Self::Exited { pid },
        }
    }
}

impl From<ListeningPort> for FfiListeningPort {
    fn from(port: ListeningPort) -> Self {
        Self {
            address: port.address,
            port: port.port,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn machine_usage_reads_without_a_process_list_and_splits_memory() {
        let session = TinySession::new();
        let first = session.machine_usage();
        assert!(first.cpu_warming_up);
        assert!(first.network_warming_up && first.network.is_none());
        let used = first.memory_used_bytes.unwrap();
        let split = first.memory_apps_bytes.unwrap()
            + first.memory_wired_bytes.unwrap()
            + first.memory_compressed_bytes.unwrap();
        assert_eq!(split, used);
    }

    #[test]
    fn cpu_is_unmeasured_until_the_third_list() {
        let session = TinySession::new();
        let first = session.process_list().unwrap();
        assert!(!first.cpu_measured);
        assert!(first.processes.iter().all(|p| p.cpu_percent.is_none()));
        std::thread::sleep(sysinfo_interval());
        assert!(!session.process_list().unwrap().cpu_measured);
        std::thread::sleep(sysinfo_interval());
        let third = session.process_list().unwrap();
        assert!(third.cpu_measured);
        let me = third
            .processes
            .iter()
            .find(|p| p.pid == std::process::id())
            .expect("own process is sampled");
        assert!(me.is_current_user);
        assert!(me.cpu_measured);
        // Only macOS reads a footprint and CPU time; elsewhere both stay unavailable.
        assert_eq!(
            me.memory_bytes.is_some_and(|bytes| bytes > 0),
            cfg!(target_os = "macos")
        );
        assert_eq!(
            me.cpu_time_ns.is_some_and(|ns| ns > 0),
            cfg!(target_os = "macos")
        );
        assert_eq!(me.refusal, Some(FfiRefusal::OwnProcess));
    }

    #[test]
    fn memory_crosses_the_boundary_as_the_footprint() {
        let info = |footprint_bytes| ProcessInfo {
            pid: 42,
            name: "sample".into(),
            user: None,
            uid: None,
            is_current_user: false,
            parent_pid: None,
            start_time: 1,
            cpu_percent: None,
            cpu_measured: false,
            memory_bytes: Some(4096),
            footprint_bytes,
            cpu_time_ns: None,
            responsible_pid: None,
            responsible_start_time: None,
            exe: None,
        };
        assert_eq!(
            FfiProcessInfo::from(info(Some(1024))).memory_bytes,
            Some(1024)
        );
        // An unread footprint never falls back to resident memory.
        assert_eq!(FfiProcessInfo::from(info(None)).memory_bytes, None);
    }

    #[test]
    fn cpu_time_crosses_the_boundary_and_unread_stays_unread() {
        let info = |cpu_time_ns| ProcessInfo {
            pid: 42,
            name: "sample".into(),
            user: None,
            uid: None,
            is_current_user: false,
            parent_pid: None,
            start_time: 1,
            cpu_percent: None,
            cpu_measured: false,
            memory_bytes: Some(4096),
            footprint_bytes: Some(1024),
            cpu_time_ns,
            responsible_pid: None,
            responsible_start_time: None,
            exe: None,
        };
        assert_eq!(
            FfiProcessInfo::from(info(Some(1_500_000_000))).cpu_time_ns,
            Some(1_500_000_000)
        );
        assert_eq!(FfiProcessInfo::from(info(None)).cpu_time_ns, None);
    }

    #[test]
    fn responsible_process_crosses_the_boundary_and_none_stays_none() {
        let info = |responsible: Option<(u32, u64)>| ProcessInfo {
            pid: 42,
            name: "VTDecoderXPCService".into(),
            user: None,
            uid: None,
            is_current_user: true,
            parent_pid: Some(1),
            start_time: 1,
            cpu_percent: None,
            cpu_measured: false,
            memory_bytes: Some(4096),
            footprint_bytes: Some(1024),
            cpu_time_ns: None,
            responsible_pid: responsible.map(|(pid, _)| pid),
            responsible_start_time: responsible.map(|(_, start)| start),
            exe: None,
        };
        let joined = FfiProcessInfo::from(info(Some((7, 1_700_000_000))));
        assert_eq!(
            (joined.responsible_pid, joined.responsible_start_time),
            (Some(7), Some(1_700_000_000))
        );
        let alone = FfiProcessInfo::from(info(None));
        assert_eq!(
            (alone.responsible_pid, alone.responsible_start_time),
            (None, None)
        );
    }

    #[test]
    fn listed_processes_carry_the_terminate_refusal() {
        let session = TinySession::new();
        let list = session.process_list().unwrap().processes;
        let launchd = list
            .iter()
            .find(|p| p.pid == 1)
            .expect("launchd is sampled");
        assert_eq!(launchd.refusal, Some(FfiRefusal::SystemProcess));
        let parent = list
            .iter()
            .find(|p| p.pid == std::os::unix::process::parent_id())
            .expect("parent is sampled");
        assert_eq!(parent.refusal, Some(FfiRefusal::OwnParent));
        let (mut child, _) = sleeper(&session);
        let listed = session.process_list().unwrap().processes;
        let sleeping = listed.iter().find(|p| p.pid == child.id()).unwrap();
        assert_eq!(sleeping.refusal, None);
        child.kill().unwrap();
        child.wait().unwrap();
    }

    #[test]
    fn detail_of_own_process_has_ports_and_unknown_pid_is_typed() {
        let session = TinySession::new();
        let detail = session.process_detail(std::process::id()).unwrap();
        assert!(detail.ports.is_some() || detail.ports_error.is_some());
        assert!(matches!(
            session.process_detail(u32::MAX),
            Err(FfiError::InvalidInput { .. })
        ));
    }

    #[test]
    fn process_queries_bypass_the_operation_gate() {
        let session = TinySession::new();
        let _gate = session.begin().unwrap();
        assert!(!session.process_list().unwrap().processes.is_empty());
    }

    /// Only disposable children are signalled.
    fn sleeper(session: &TinySession) -> (std::process::Child, FfiTerminateTarget) {
        let child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let info = session
            .process_list()
            .unwrap()
            .processes
            .into_iter()
            .find(|p| p.pid == child.id())
            .expect("child is sampled");
        let target = FfiTerminateTarget {
            pid: info.pid,
            start_time: info.start_time,
            name: info.name,
            executable_path: info.executable_path,
        };
        (child, target)
    }

    /// A scan holds the gate for minutes; Quit of a runaway process must not wait for it.
    #[test]
    fn terminate_quits_the_child_while_a_scan_holds_the_gate() {
        let session = TinySession::new();
        let (mut child, target) = sleeper(&session);
        let gate = session.begin().unwrap();
        let outcome = session.process_terminate(target, FfiTerminateKind::Graceful);
        let still_held = session.is_busy();
        drop(gate);
        child.kill().ok();
        child.wait().unwrap();
        assert_eq!(outcome.unwrap(), FfiTerminateOutcome::Exited);
        assert!(still_held);
    }

    #[test]
    fn terminate_refusal_crosses_the_boundary_as_a_value() {
        let session = TinySession::new();
        let target = FfiTerminateTarget {
            pid: 1,
            start_time: 0,
            name: "launchd".into(),
            executable_path: None,
        };
        assert_eq!(
            session
                .process_terminate(target, FfiTerminateKind::Force)
                .unwrap(),
            FfiTerminateOutcome::Refused {
                reason: FfiRefusal::SystemProcess
            }
        );
    }

    #[test]
    fn port_owners_bypass_the_gate_and_reject_port_zero() {
        let session = TinySession::new();
        let _gate = session.begin().unwrap();
        assert!(matches!(
            session.process_port_owners(0),
            Err(FfiError::InvalidInput { .. })
        ));
        let found = session.process_port_owners(1).unwrap();
        assert!(!found.visibility_caveat.is_empty());
    }

    #[test]
    fn listeners_include_a_disposable_listener_while_the_gate_is_held() {
        use std::io::{BufRead, BufReader};
        let script = "import socket,time\ns=socket.socket()\ns.bind(('127.0.0.1',0))\ns.listen()\nprint(s.getsockname()[1],flush=True)\ntime.sleep(30)";
        let Ok(mut child) = std::process::Command::new("python3")
            .args(["-c", script])
            .stdout(std::process::Stdio::piped())
            .spawn()
        else {
            eprintln!("python3 unavailable; listeners are covered by core fixtures only");
            return;
        };
        let mut line = String::new();
        let read = BufReader::new(child.stdout.take().unwrap()).read_line(&mut line);
        let session = TinySession::new();
        let gate = session.begin().unwrap();
        let found = session.process_listeners();
        drop(gate);
        child.kill().unwrap();
        child.wait().unwrap();
        read.unwrap();
        let port: u16 = line.trim().parse().unwrap();
        let found = found.unwrap();
        let mine = found
            .listeners
            .iter()
            .find(|l| l.port == port)
            .expect("listener is visible");
        assert_eq!(mine.pid, child.id());
        assert!(mine.actionable);
        assert!(!found.visibility_caveat.is_empty());
    }

    fn sysinfo_interval() -> std::time::Duration {
        std::time::Duration::from_millis(250)
    }
}
