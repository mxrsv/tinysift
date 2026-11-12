//! Process sampling. Missing measurements stay explicit (`None`) instead of
//! being reported as freshly measured zeroes.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind, Users};

use super::network::{NetworkCounters, NetworkReading, NetworkThroughput};
use super::pressure::{read_memory_pressure, MemoryPressure};
use crate::sys::MemoryParts;

/// One running process: its PID plus its start time, so a reused PID never matches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProcessKey {
    pub pid: u32,
    /// Seconds since the Unix epoch, 1 s granularity.
    pub start_time: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessInfo {
    pub pid: u32,
    pub name: String,
    pub user: Option<String>,
    #[serde(skip)]
    pub uid: Option<u32>,
    /// Both the real and the effective UID are the current user's.
    pub is_current_user: bool,
    pub parent_pid: Option<u32>,
    /// Seconds since the Unix epoch, 1 s granularity.
    pub start_time: u64,
    /// Per-core percentage as reported by `sysinfo`; can exceed 100 on
    /// multi-core machines. `None` until measured or when access is denied.
    pub cpu_percent: Option<f32>,
    pub cpu_measured: bool,
    /// Resident memory. `None` when the process cannot be inspected.
    pub memory_bytes: Option<u64>,
    /// Physical footprint: dirty private memory plus its compressed and swapped pages,
    /// without clean file-backed or shared library pages, as `top`'s MEM column reports.
    /// Filled by `Sampler::snapshot`; `to_info` leaves it `None`. Native app only;
    /// preserve the existing CLI JSON contract.
    #[serde(skip)]
    pub footprint_bytes: Option<u64>,
    /// User plus system CPU time since the process started, including its reaped
    /// children's, in nanoseconds. Read with the footprint and `None` exactly when it is.
    /// Native app only; preserve the existing CLI JSON contract.
    #[serde(skip)]
    pub cpu_time_ns: Option<u64>,
    /// The process macOS holds responsible for this one (the app an XPC service or helper works
    /// for), with its start time so a reused PID never matches. Filled by `Sampler::snapshot`
    /// only when that process is in the same sample and is not this process; `None` otherwise,
    /// including when macOS gives no answer. Native app only; preserve the CLI JSON contract
    /// (`ProcessKey` has no `Serialize`, so dropping the `skip` does not compile).
    #[serde(skip)]
    pub responsible: Option<ProcessKey>,
    #[serde(skip)]
    pub exe: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProcessSnapshot {
    pub processes: Vec<ProcessInfo>,
    /// Seconds since the Unix epoch.
    pub sampled_at: u64,
    /// True once every process present since the first sample has measured
    /// CPU. Processes that appeared later report their own `cpu_measured`.
    pub cpu_measured: bool,
    /// Native overview only; preserve the existing CLI JSON contract.
    #[serde(skip)]
    pub system_usage: SystemUsage,
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct LoadAverage {
    pub one: f64,
    pub five: f64,
    pub fifteen: f64,
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct SystemUsage {
    /// Whole-machine utilization, unlike per-core process percentages.
    pub cpu_percent: Option<f32>,
    pub cpu_warming_up: bool,
    /// Each core 0–100, `None` for a core that did not report. Empty while warming up.
    pub cpu_cores: Vec<Option<f32>>,
    pub cpu_brand: Option<String>,
    pub logical_cores: Option<u32>,
    pub load_average: Option<LoadAverage>,
    pub memory_used_bytes: Option<u64>,
    pub memory_total_bytes: Option<u64>,
    /// The used figure split into app, wired and compressed memory; `None` where the
    /// platform cannot split it. When present, the three sum to `memory_used_bytes`.
    pub memory_apps_bytes: Option<u64>,
    pub memory_wired_bytes: Option<u64>,
    pub memory_compressed_bytes: Option<u64>,
    /// `Some(0)` for both means swap is not in use.
    pub swap_used_bytes: Option<u64>,
    pub swap_total_bytes: Option<u64>,
    pub memory_pressure: Option<MemoryPressure>,
    /// Bytes per second over the counted interfaces since the previous sample. `None`
    /// while the first sample sets the baseline (`network_warming_up`) or when no
    /// interface could be read.
    pub network: Option<NetworkThroughput>,
    pub network_warming_up: bool,
}

/// Raw whole-machine values, before availability rules are applied.
#[derive(Default)]
struct SystemReadings {
    cpu: f32,
    cpu_measured: bool,
    cores: Vec<f32>,
    brand: String,
    load: [f64; 3],
    memory_total: u64,
    memory_used: Option<u64>,
    memory_parts: Option<MemoryParts>,
    swap_used: u64,
    swap_total: u64,
    pressure: Option<MemoryPressure>,
    network: NetworkReading,
}

impl SystemUsage {
    fn from_readings(raw: SystemReadings) -> Self {
        let memory_ok =
            raw.memory_total > 0 && raw.memory_used.is_some_and(|used| used <= raw.memory_total);
        let parts = raw.memory_parts.filter(|_| memory_ok);
        let swap_ok = raw.swap_used <= raw.swap_total;
        let load_ok = raw.load.iter().all(|v| v.is_finite() && *v >= 0.0);
        let [one, five, fifteen] = raw.load;
        Self {
            cpu_percent: (raw.cpu_measured && raw.cpu.is_finite())
                .then_some(raw.cpu.clamp(0.0, 100.0)),
            cpu_warming_up: !raw.cpu_measured,
            cpu_cores: if raw.cpu_measured {
                raw.cores
                    .iter()
                    .map(|c| c.is_finite().then_some(c.clamp(0.0, 100.0)))
                    .collect()
            } else {
                Vec::new()
            },
            cpu_brand: (!raw.cores.is_empty() && !raw.brand.is_empty()).then_some(raw.brand),
            logical_cores: u32::try_from(raw.cores.len()).ok().filter(|n| *n > 0),
            load_average: load_ok.then_some(LoadAverage { one, five, fifteen }),
            memory_used_bytes: raw.memory_used.filter(|_| memory_ok),
            memory_total_bytes: memory_ok.then_some(raw.memory_total),
            memory_apps_bytes: parts.map(|p| p.apps),
            memory_wired_bytes: parts.map(|p| p.wired),
            memory_compressed_bytes: parts.map(|p| p.compressed),
            swap_used_bytes: swap_ok.then_some(raw.swap_used),
            swap_total_bytes: swap_ok.then_some(raw.swap_total),
            memory_pressure: raw.pressure,
            network: raw.network.throughput,
            network_warming_up: raw.network.warming_up,
        }
    }
}

/// Raw values for one process, before availability rules are applied.
pub(crate) struct RawProcess {
    pub pid: u32,
    pub name: String,
    pub uid: Option<u32>,
    pub effective_uid: Option<u32>,
    pub user: Option<String>,
    pub parent_pid: Option<u32>,
    pub start_time: u64,
    pub cpu: f32,
    pub memory: u64,
    pub exe: Option<PathBuf>,
}

pub(crate) fn raw_process(process: &sysinfo::Process, user: Option<String>) -> RawProcess {
    RawProcess {
        pid: process.pid().as_u32(),
        name: process.name().to_string_lossy().into_owned(),
        user,
        uid: process.user_id().map(|uid| **uid),
        effective_uid: process.effective_user_id().map(|uid| **uid),
        parent_pid: process.parent().map(|pid| pid.as_u32()),
        start_time: process.start_time(),
        cpu: process.cpu_usage(),
        memory: process.memory(),
        exe: process.exe().map(PathBuf::from),
    }
}

/// Applies the availability rules. A live process never has zero resident
/// memory, so `memory == 0` means macOS denied task info (another user's
/// process); CPU from the same call is then unavailable too.
pub(crate) fn to_info(raw: RawProcess, cpu_measured: bool, current_uid: u32) -> ProcessInfo {
    let readable = raw.memory > 0;
    let measured = cpu_measured && readable;
    ProcessInfo {
        pid: raw.pid,
        name: raw.name,
        user: raw.user,
        // A setuid process (ruid = me, euid = root) is not the current user's.
        is_current_user: raw.uid == Some(current_uid) && raw.effective_uid == Some(current_uid),
        uid: raw.uid,
        parent_pid: raw.parent_pid,
        start_time: raw.start_time,
        cpu_percent: measured.then_some(raw.cpu),
        cpu_measured: measured,
        memory_bytes: readable.then_some(raw.memory),
        footprint_bytes: None,
        cpu_time_ns: None,
        responsible: None,
        exe: raw.exe,
    }
}

/// `sysinfo` on macOS records a new process's CPU times on its second refresh
/// and reports a real value only from the third; earlier values read 0.
const REFRESHES_FOR_CPU: u32 = 3;

/// Processes the PID-set read may hold beyond the pids it was asked for before it starts afresh.
/// Starting afresh restarts every followed pid's three-read warm-up, so it should be rare.
const PID_READ_STALE_LIMIT: usize = 256;

/// One pid as earlier refreshes saw it.
struct Seen {
    start: u64,
    /// Refreshes this process has been seen in.
    count: u32,
    exe: Option<PathBuf>,
    /// The name after an `exec` under the same pid and start time, which the
    /// shared `System` would otherwise keep from before it.
    name: Option<String>,
    /// macOS's answer for the responsible process, read once per pid and start time: it is
    /// set when the process is spawned and does not change after.
    responsible: Option<u32>,
}

/// The name a fresh lookup reports for `pid`, as `terminate` sees it.
fn fresh_name(pid: u32) -> Option<String> {
    let pid = sysinfo::Pid::from_u32(pid);
    let mut system = System::new();
    system.refresh_processes_specifics(
        ProcessesToUpdate::Some(&[pid]),
        true,
        ProcessRefreshKind::new().with_exe(UpdateKind::Always),
    );
    system
        .process(pid)
        .map(|process| process.name().to_string_lossy().into_owned())
}

/// Which processes a refresh reads.
#[derive(Clone, Copy)]
enum Scope<'a> {
    All,
    Pids(&'a [u32]),
}

/// One `System` and what its refreshes saw. A process's CPU percent is its CPU time over the
/// interval since this reader's previous refresh, so the full read and the PID-set read each own
/// one: a read of a few PIDs every 2 s must not shorten the interval the full read divides by.
struct ProcessReader {
    system: System,
    refreshes: u32,
    /// What earlier refreshes saw of each pid read; its keys are the processes of the last read.
    seen: HashMap<u32, Seen>,
}

impl ProcessReader {
    fn new() -> Self {
        Self {
            system: System::new(),
            refreshes: 0,
            seen: HashMap::new(),
        }
    }

    fn refresh(&mut self, scope: Scope) {
        let kind = ProcessRefreshKind::new()
            .with_cpu()
            .with_memory()
            .with_user(UpdateKind::OnlyIfNotSet)
            .with_exe(UpdateKind::Always);
        match scope {
            Scope::All => {
                self.system
                    .refresh_processes_specifics(ProcessesToUpdate::All, true, kind);
            }
            Scope::Pids(pids) => {
                // `sysinfo` forgets a pid only when a read asks for it and finds it gone, so the
                // pids earlier panels followed would pile up in a long-running app.
                if self.system.processes().len() > pids.len() + PID_READ_STALE_LIMIT {
                    *self = Self::new();
                }
                let pids: Vec<_> = pids.iter().copied().map(sysinfo::Pid::from_u32).collect();
                self.system
                    .refresh_processes_specifics(ProcessesToUpdate::Some(&pids), true, kind);
            }
        }
        self.refreshes = self.refreshes.saturating_add(1);
        self.track_seen(scope);
    }

    /// Rebuilds `seen` from the processes this refresh read. A pid continues its count only if the
    /// previous refresh read it too: `sysinfo` keeps the CPU times of a pid left out of a PID-set
    /// read, so the first interval after a gap spans the gap and is not reported as measured.
    fn track_seen(&mut self, scope: Scope) {
        let processes: Vec<&sysinfo::Process> = match scope {
            Scope::All => self.system.processes().values().collect(),
            Scope::Pids(pids) => pids
                .iter()
                .filter_map(|pid| self.system.process(sysinfo::Pid::from_u32(*pid)))
                .collect(),
        };
        let mut seen = HashMap::with_capacity(processes.len());
        for process in processes {
            let pid = process.pid().as_u32();
            let start = process.start_time();
            let exe = process.exe().map(PathBuf::from);
            let entry = match self.seen.get(&pid) {
                Some(previous) if previous.start == start => Seen {
                    start,
                    count: previous.count + 1,
                    responsible: previous.responsible,
                    // `sysinfo` keeps a known process's name across `exec`;
                    // only a fresh lookup reads the new one.
                    name: if previous.exe.is_some() && previous.exe != exe {
                        fresh_name(pid)
                    } else {
                        previous.name.clone()
                    },
                    exe,
                },
                _ => Seen {
                    start,
                    count: 1,
                    exe,
                    name: None,
                    responsible: super::responsible::read(pid),
                },
            };
            seen.insert(pid, entry);
        }
        self.seen = seen;
    }

    /// The responsible process of `pid` as a PID plus start time, when this read has it and it
    /// is not `pid` itself.
    fn responsible_of(&self, pid: u32) -> Option<ProcessKey> {
        let responsible = self.seen.get(&pid)?.responsible.filter(|r| *r != pid)?;
        let start_time = self.seen.get(&responsible)?.start;
        Some(ProcessKey {
            pid: responsible,
            start_time,
        })
    }
}

/// Keeps its `System`s alive so later samples have measured CPU. Three reads, each at its own pace:
/// [`Sampler::sample`] walks every process, [`Sampler::sample_pids`] reads a few, and the
/// whole-machine figures alone come from [`super::MachineSampler`], which walks none.
pub struct Sampler {
    /// Every process; also holds the machine's CPU and memory for `system_usage`.
    all: ProcessReader,
    /// The PID-set read's own `System`, so it never moves `all`'s CPU interval.
    pids: ProcessReader,
    users: Users,
    current_uid: u32,
    system_cpu_samples: u32,
    last_system_cpu_sample: Option<Instant>,
    network: NetworkCounters,
    network_reading: NetworkReading,
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Sampler {
    pub fn new() -> Self {
        Self {
            all: ProcessReader::new(),
            pids: ProcessReader::new(),
            users: Users::new_with_refreshed_list(),
            system_cpu_samples: 0,
            last_system_cpu_sample: None,
            network: NetworkCounters::new(),
            network_reading: NetworkReading::default(),
            // SAFETY: getuid has no preconditions and cannot fail.
            current_uid: unsafe { libc::getuid() },
        }
    }

    pub fn current_uid(&self) -> u32 {
        self.current_uid
    }

    /// Refreshes every process once. A process has measured CPU from its third sample on, the
    /// average over the interval since the previous `sample`.
    pub fn sample(&mut self) -> ProcessSnapshot {
        self.refresh_system_usage();
        self.all.refresh(Scope::All);
        ProcessSnapshot {
            processes: self.infos(&self.all),
            sampled_at: unix_now(),
            cpu_measured: self.all.refreshes >= REFRESHES_FOR_CPU,
            system_usage: self.system_usage(),
        }
    }

    /// Reads only `pids`, for a panel that follows a few processes between full samples. Leaves
    /// out a pid that is gone. CPU is the average since the previous `sample_pids` that read the
    /// same pid, measured from the third consecutive read that includes it; `responsible` is set
    /// only when the responsible process is among `pids`. Never touches `sample`'s intervals.
    pub fn sample_pids(&mut self, pids: &[u32]) -> Vec<ProcessInfo> {
        self.pids.refresh(Scope::Pids(pids));
        self.infos(&self.pids)
    }

    fn refresh_system_usage(&mut self) {
        let system = &mut self.all.system;
        system.refresh_memory();
        let now = Instant::now();
        let due = self.last_system_cpu_sample.is_none_or(|previous| {
            now.duration_since(previous) >= sysinfo::MINIMUM_CPU_UPDATE_INTERVAL
        });
        if due {
            system.refresh_cpu_usage();
            self.system_cpu_samples = self.system_cpu_samples.saturating_add(1);
            self.last_system_cpu_sample = Some(now);
        }
        self.network_reading = self.network.sample();
    }

    /// For one-shot callers such as the CLI: refreshes until CPU is measured,
    /// waiting the minimum CPU interval between refreshes (about 0.4 s).
    pub fn sample_measured(&mut self) -> ProcessSnapshot {
        while self.all.refreshes + 1 < REFRESHES_FOR_CPU {
            self.sample();
            std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        }
        self.sample()
    }

    fn system_usage(&self) -> SystemUsage {
        usage_of(
            &self.all.system,
            self.system_cpu_samples,
            self.network_reading,
        )
    }

    /// The processes `reader`'s last refresh read, with footprint, CPU time and responsible process.
    fn infos(&self, reader: &ProcessReader) -> Vec<ProcessInfo> {
        reader
            .seen
            .iter()
            .filter_map(|(pid, seen)| {
                let process = reader.system.process(sysinfo::Pid::from_u32(*pid))?;
                let user = process
                    .user_id()
                    .and_then(|uid| self.users.get_user_by_id(uid))
                    .map(|user| user.name().to_string());
                let mut raw = raw_process(process, user);
                if let Some(name) = seen.name.clone() {
                    raw.name = name;
                }
                let info = to_info(raw, seen.count >= REFRESHES_FOR_CPU, self.current_uid);
                // Only a process `to_info` could read gets a footprint and CPU time, so a
                // missing memory reading keeps meaning "unreadable" across the boundary.
                let usage = info
                    .memory_bytes
                    .map(|_| super::footprint::read(info.pid))
                    .unwrap_or_default();
                Some(ProcessInfo {
                    footprint_bytes: usage.footprint_bytes,
                    cpu_time_ns: usage.cpu_time_ns,
                    responsible: reader.responsible_of(info.pid),
                    ..info
                })
            })
            .collect()
    }
}

/// Whole-machine readings from a `System` whose CPU and memory were just refreshed.
/// `cpu_samples` counts CPU refreshes; one is not enough to measure a rate.
pub(super) fn usage_of(system: &System, cpu_samples: u32, network: NetworkReading) -> SystemUsage {
    let cpus = system.cpus();
    let load = System::load_average();
    // One `host_statistics64` call feeds both the used figure and its split.
    let parts = crate::sys::memory_parts();
    #[cfg(target_os = "macos")]
    let memory_used = parts.map(MemoryParts::used);
    #[cfg(not(target_os = "macos"))]
    let memory_used = Some(system.used_memory());
    SystemUsage::from_readings(SystemReadings {
        cpu: if cpus.is_empty() {
            f32::NAN
        } else {
            system.global_cpu_usage()
        },
        cpu_measured: cpu_samples >= 2,
        cores: cpus.iter().map(|cpu| cpu.cpu_usage()).collect(),
        brand: cpus
            .first()
            .map_or_else(String::new, |c| c.brand().to_string()),
        load: [load.one, load.five, load.fifteen],
        memory_total: system.total_memory(),
        memory_used,
        memory_parts: parts,
        swap_used: system.used_swap(),
        swap_total: system.total_swap(),
        pressure: read_memory_pressure(),
        network,
    })
}

pub(crate) fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests;
