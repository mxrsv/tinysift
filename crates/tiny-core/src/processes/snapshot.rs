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
mod tests {
    use super::*;

    fn raw(memory: u64, uid: u32) -> RawProcess {
        RawProcess {
            pid: 42,
            name: "sample".into(),
            uid: Some(uid),
            effective_uid: Some(uid),
            user: Some("me".into()),
            parent_pid: Some(1),
            start_time: 1_700_000_000,
            cpu: 12.5,
            memory,
            exe: None,
        }
    }

    fn readings() -> SystemReadings {
        SystemReadings {
            cpu_measured: true,
            cores: vec![10.0, 20.0],
            brand: "Apple M3 Pro".into(),
            load: [1.0, 2.0, 3.0],
            memory_total: 1000,
            memory_used: Some(250),
            swap_total: 0,
            ..Default::default()
        }
    }

    #[test]
    fn system_usage_preserves_warmup_and_memory_bounds() {
        let first = SystemUsage::from_readings(SystemReadings {
            cpu: 0.0,
            cpu_measured: false,
            ..readings()
        });
        assert_eq!(first.cpu_percent, None);
        assert!(first.cpu_warming_up);
        assert!(first.cpu_cores.is_empty());
        assert_eq!(first.logical_cores, Some(2));
        assert_eq!(first.memory_used_bytes, Some(250));
        assert_eq!(first.memory_total_bytes, Some(1000));
        let unavailable = SystemUsage::from_readings(SystemReadings {
            cpu: f32::NAN,
            cpu_measured: true,
            ..Default::default()
        });
        assert_eq!(
            unavailable,
            SystemUsage {
                swap_used_bytes: Some(0),
                swap_total_bytes: Some(0),
                load_average: Some(LoadAverage {
                    one: 0.0,
                    five: 0.0,
                    fifteen: 0.0
                }),
                ..Default::default()
            }
        );
        let clamped = SystemUsage::from_readings(SystemReadings {
            cpu: 120.0,
            ..readings()
        });
        assert_eq!(clamped.cpu_percent, Some(100.0));
        let overfull = SystemUsage::from_readings(SystemReadings {
            memory_total: 10,
            memory_used: Some(11),
            ..readings()
        });
        assert_eq!(overfull.memory_used_bytes, None);
    }

    #[test]
    fn per_core_values_are_clamped_and_non_finite_is_unavailable() {
        let usage = SystemUsage::from_readings(SystemReadings {
            cores: vec![-5.0, 50.0, 140.0, f32::NAN, f32::INFINITY],
            ..readings()
        });
        assert_eq!(
            usage.cpu_cores,
            vec![Some(0.0), Some(50.0), Some(100.0), None, None]
        );
        assert_eq!(usage.logical_cores, Some(5));
        assert_eq!(usage.cpu_brand.as_deref(), Some("Apple M3 Pro"));
    }

    #[test]
    fn no_cpus_or_empty_brand_leaves_cpu_identity_unavailable() {
        let none = SystemUsage::from_readings(SystemReadings {
            cores: vec![],
            ..readings()
        });
        assert_eq!((none.logical_cores, none.cpu_brand), (None, None));
        let blank = SystemUsage::from_readings(SystemReadings {
            brand: String::new(),
            ..readings()
        });
        assert_eq!(blank.cpu_brand, None);
    }

    #[test]
    fn swap_total_zero_is_available_and_used_over_total_is_not() {
        let none = SystemUsage::from_readings(readings());
        assert_eq!(
            (none.swap_used_bytes, none.swap_total_bytes),
            (Some(0), Some(0))
        );
        let some = SystemUsage::from_readings(SystemReadings {
            swap_used: 5,
            swap_total: 8,
            ..readings()
        });
        assert_eq!(
            (some.swap_used_bytes, some.swap_total_bytes),
            (Some(5), Some(8))
        );
        let bad = SystemUsage::from_readings(SystemReadings {
            swap_used: 9,
            swap_total: 8,
            ..readings()
        });
        assert_eq!((bad.swap_used_bytes, bad.swap_total_bytes), (None, None));
    }

    #[test]
    fn load_average_rejects_non_finite_and_negative_values() {
        assert_eq!(
            SystemUsage::from_readings(readings()).load_average,
            Some(LoadAverage {
                one: 1.0,
                five: 2.0,
                fifteen: 3.0
            })
        );
        for bad in [f64::NAN, f64::INFINITY, -0.1] {
            let usage = SystemUsage::from_readings(SystemReadings {
                load: [1.0, bad, 3.0],
                ..readings()
            });
            assert_eq!(usage.load_average, None);
        }
    }

    #[test]
    fn pressure_reading_passes_through() {
        let usage = SystemUsage::from_readings(SystemReadings {
            pressure: Some(MemoryPressure::Warning),
            ..readings()
        });
        assert_eq!(usage.memory_pressure, Some(MemoryPressure::Warning));
    }

    #[test]
    fn system_usage_is_live_without_changing_cli_json() {
        let mut sampler = Sampler::new();
        let first = sampler.sample();
        assert_eq!(first.system_usage.cpu_percent, None);
        assert!(first.system_usage.cpu_cores.is_empty());
        assert!(first
            .system_usage
            .memory_total_bytes
            .is_some_and(|total| total > 0));
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        let next = sampler.sample();
        assert!(next
            .system_usage
            .cpu_percent
            .is_some_and(|cpu| (0.0..=100.0).contains(&cpu)));
        let usage = &next.system_usage;
        assert!(usage.logical_cores.is_some_and(|n| n > 0));
        assert_eq!(usage.cpu_cores.len(), usage.logical_cores.unwrap() as usize);
        let json = serde_json::to_value(next).unwrap();
        assert!(json.get("systemUsage").is_none());
    }

    /// Use a child test process so occupying Rayon's global pool cannot delay
    /// unrelated tests. A scan worker held at a checkpoint must not stop sampling.
    #[test]
    #[cfg(target_os = "macos")]
    fn sampler_does_not_wait_for_scan_workers() {
        const CHILD: &str = "TINY_SAMPLER_CONTENTION_TEST";
        if std::env::var_os(CHILD).is_some() {
            sample_with_occupied_scan_pool();
            return;
        }
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "processes::snapshot::tests::sampler_does_not_wait_for_scan_workers",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .output()
            .unwrap();
        let stdout = String::from_utf8_lossy(&output.stdout);
        print!("{stdout}");
        eprint!("{}", String::from_utf8_lossy(&output.stderr));
        assert!(
            output.status.success(),
            "sampling waited for the occupied scan pool"
        );
        assert!(
            stdout.contains("Sample completed with scan pool occupied"),
            "the child must execute the regression, not pass with zero matching tests"
        );
    }

    #[cfg(target_os = "macos")]
    fn sample_with_occupied_scan_pool() {
        use std::sync::mpsc;
        use std::time::Duration;

        const WORKER_START_TIMEOUT: Duration = Duration::from_secs(5);
        const SAMPLE_TIMEOUT: Duration = Duration::from_secs(2);
        rayon::ThreadPoolBuilder::new()
            .num_threads(1)
            .build_global()
            .unwrap();
        let (started, ready) = mpsc::channel();
        let (release, held) = mpsc::channel();
        rayon::spawn(move || {
            started.send(()).unwrap();
            held.recv().unwrap();
        });
        ready.recv_timeout(WORKER_START_TIMEOUT).unwrap();
        let (send, receive) = mpsc::channel();
        let sampling = std::thread::spawn(move || {
            let mut sampler = Sampler::new();
            let start = Instant::now();
            let snapshot = sampler.sample();
            let contains_self = snapshot
                .processes
                .iter()
                .any(|p| p.pid == std::process::id());
            send.send((start.elapsed(), contains_self)).unwrap();
        });
        let result = receive.recv_timeout(SAMPLE_TIMEOUT);
        // Always unblock and join before asserting, including the failing case.
        release.send(()).unwrap();
        sampling.join().unwrap();
        let (elapsed, contains_self) =
            result.expect("sample must finish while scan workers are held");
        assert!(
            contains_self,
            "the independent sample still contains real processes"
        );
        println!("Sample completed with scan pool occupied in {elapsed:?}");
    }

    #[test]
    fn executable_path_refreshes_after_same_identity_exec() {
        use std::io::Write;
        use std::process::{Command, Stdio};
        use std::time::Duration;

        let mut child = Command::new("/bin/sh")
            .args(["-c", "read marker; exec /bin/sleep 30"])
            .stdin(Stdio::piped())
            .spawn()
            .unwrap();
        let pid = child.id();
        let mut sampler = Sampler::new();
        let sample = sampler.sample();
        let before = sample.processes.into_iter().find(|p| p.pid == pid);
        let write_result = child.stdin.take().unwrap().write_all(b"go\n");
        let deadline = Instant::now() + Duration::from_secs(3);
        let mut after;
        loop {
            let sample = sampler.sample();
            after = sample.processes.into_iter().find(|p| p.pid == pid);
            let changed = after
                .as_ref()
                .and_then(|p| p.exe.as_ref())
                .and_then(|path| path.file_name())
                .is_some_and(|name| name == "sleep");
            if changed || Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        // Clean up only our child before making assertions, including the failing case.
        child.kill().unwrap();
        child.wait().unwrap();
        write_result.unwrap();
        let before = before.expect("shell appears before exec");
        let after = after.expect("same process appears after exec");
        assert_eq!(before.pid, after.pid);
        assert_eq!(before.start_time, after.start_time);
        assert!(before.exe.is_some());
        assert_ne!(
            before.exe.as_ref().and_then(|path| path.file_name()),
            Some(std::ffi::OsStr::new("sleep"))
        );
        assert_eq!(
            after.exe.as_ref().and_then(|path| path.file_name()),
            Some(std::ffi::OsStr::new("sleep"))
        );
        // The name follows the exec too: `terminate` probes a fresh `System`,
        // so a name kept from before the exec would read as a changed identity.
        assert_ne!(before.name, "sleep");
        assert_eq!(after.name, "sleep");
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn a_child_reports_the_tests_responsible_process_with_its_start_time() {
        use std::process::{Command, Stdio};

        let mut child = Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let pid = child.id();
        let sample = Sampler::new().sample();
        child.kill().unwrap();
        child.wait().unwrap();
        let find = |pid| sample.processes.iter().find(|p| p.pid == pid);
        let child = find(pid).expect("the child is sampled");
        let expected = super::super::responsible::read(std::process::id())
            .filter(|responsible| *responsible != pid)
            .and_then(find)
            .map(|p| ProcessKey {
                pid: p.pid,
                start_time: p.start_time,
            });
        assert_eq!(child.responsible, expected);
        // A process never names itself.
        for process in &sample.processes {
            assert_ne!(process.responsible.map(|key| key.pid), Some(process.pid));
        }
    }

    /// A child that keeps one core busy until killed.
    #[cfg(target_os = "macos")]
    fn busy_child() -> std::process::Child {
        use std::process::{Command, Stdio};
        Command::new("/usr/bin/yes")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
            .unwrap()
    }

    #[cfg(target_os = "macos")]
    fn cpu_of(processes: &[ProcessInfo], pid: u32) -> Option<f32> {
        processes
            .iter()
            .find(|p| p.pid == pid)
            .and_then(|p| p.cpu_percent)
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn pid_read_returns_only_the_pids_asked_for_and_leaves_out_gone_ones() {
        use std::process::{Command, Stdio};

        let mut child = Command::new("/bin/sleep")
            .arg("30")
            .stdin(Stdio::null())
            .spawn()
            .unwrap();
        let mut gone = Command::new("/usr/bin/true").spawn().unwrap();
        let (pid, gone_pid, own) = (child.id(), gone.id(), std::process::id());
        gone.wait().unwrap();
        let mut sampler = Sampler::new();
        let first = sampler.sample_pids(&[pid, gone_pid, own]);
        let mut pids: Vec<u32> = first.iter().map(|p| p.pid).collect();
        pids.sort_unstable();
        let mut expected = vec![pid, own];
        expected.sort_unstable();
        assert_eq!(pids, expected);
        assert!(first
            .iter()
            .all(|p| !p.cpu_measured && p.cpu_percent.is_none()));
        // Narrowing the set leaves the other pid out of the next read.
        let second = sampler.sample_pids(&[pid]);
        child.kill().unwrap();
        child.wait().unwrap();
        assert_eq!(second.iter().map(|p| p.pid).collect::<Vec<_>>(), vec![pid]);
        assert!(second[0].footprint_bytes.is_some());
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn pid_reads_do_not_shorten_the_full_reads_interval() {
        use std::time::Duration;

        let mut busy = busy_child();
        let mut sampler = Sampler::new();
        let own = std::process::id();
        for _ in 0..REFRESHES_FOR_CPU {
            sampler.sample();
            std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        }
        // Panel reads of another pid between two full reads, the last just before the full one.
        for _ in 0..5 {
            std::thread::sleep(Duration::from_millis(250));
            sampler.sample_pids(&[own]);
        }
        let full = sampler.sample();
        busy.kill().unwrap();
        busy.wait().unwrap();
        let cpu = cpu_of(&full.processes, busy.id()).expect("the busy child is measured");
        // `yes` is one thread: at most one core. Had the panel's reads moved the full read's
        // clock, about 1.25 s of CPU time would be divided by about 0.25 s.
        assert!(
            cpu <= 130.0,
            "full read reported {cpu}% for one busy thread"
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn cpu_over_a_long_interval_is_the_average_not_a_spike() {
        use std::time::Duration;

        let mut busy = busy_child();
        let pid = busy.id();
        let mut sampler = Sampler::new();
        for _ in 0..REFRESHES_FOR_CPU {
            sampler.sample_pids(&[pid]);
            std::thread::sleep(Duration::from_millis(250));
        }
        let warmed = sampler.sample_pids(&[pid]);
        std::thread::sleep(Duration::from_secs(1));
        // SAFETY: `kill` with a valid pid of our own child has no memory-safety preconditions.
        unsafe { libc::kill(pid as libc::pid_t, libc::SIGSTOP) };
        std::thread::sleep(Duration::from_secs(1));
        let after = sampler.sample_pids(&[pid]);
        busy.kill().unwrap();
        busy.wait().unwrap();
        assert!(cpu_of(&warmed, pid).is_some_and(|cpu| cpu > 50.0));
        // Busy for half the interval, stopped for the other half.
        let cpu = cpu_of(&after, pid).expect("the child is measured");
        assert!(
            (15.0..=85.0).contains(&cpu),
            "expected about 50%, read {cpu}%"
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn pid_read_starts_afresh_once_it_holds_too_many_stale_pids() {
        let mut sampler = Sampler::new();
        sampler.pids.refresh(Scope::All);
        let held = sampler.pids.system.processes().len();
        let own = std::process::id();
        let read = sampler.sample_pids(&[own]);
        assert_eq!(read.len(), 1);
        if held > 1 + PID_READ_STALE_LIMIT {
            assert_eq!(sampler.pids.system.processes().len(), 1);
            assert_eq!(sampler.pids.refreshes, 1);
        }
    }

    #[test]
    fn first_sample_has_no_cpu_value() {
        let info = to_info(raw(4096, 501), false, 501);
        assert_eq!(info.cpu_percent, None);
        assert!(!info.cpu_measured);
        assert_eq!(info.memory_bytes, Some(4096));
        assert!(info.is_current_user);
    }

    #[test]
    fn unreadable_process_reports_unavailable_not_zero() {
        let info = to_info(raw(0, 0), true, 501);
        assert_eq!(info.memory_bytes, None);
        assert_eq!(info.cpu_percent, None);
        assert!(!info.cpu_measured);
        assert!(!info.is_current_user);
    }

    #[test]
    fn setuid_process_is_not_the_current_users() {
        let mut setuid = raw(4096, 501);
        setuid.effective_uid = Some(0);
        assert!(!to_info(setuid, true, 501).is_current_user);
        let mut unknown = raw(4096, 501);
        unknown.effective_uid = None;
        assert!(!to_info(unknown, true, 501).is_current_user);
    }

    #[test]
    fn cli_json_keeps_resident_memory_and_hides_the_native_fields() {
        let info = ProcessInfo {
            footprint_bytes: Some(1024),
            cpu_time_ns: Some(2048),
            responsible: Some(ProcessKey {
                pid: 7,
                start_time: 1,
            }),
            ..to_info(raw(4096, 501), true, 501)
        };
        let snapshot = ProcessSnapshot {
            processes: vec![info],
            sampled_at: 1,
            cpu_measured: true,
            system_usage: SystemUsage::default(),
        };
        let json = serde_json::to_value(snapshot).unwrap();
        let process = &json["processes"][0];
        assert_eq!(process["memoryBytes"], 4096);
        assert!(process.get("footprintBytes").is_none());
        assert!(process.get("cpuTimeNs").is_none());
        assert!(process.get("responsible").is_none());
    }

    #[test]
    fn measured_sample_keeps_cpu_value() {
        let info = to_info(raw(4096, 501), true, 501);
        assert_eq!(info.cpu_percent, Some(12.5));
        assert!(info.cpu_measured);
    }
}
