//! Process sampling. Missing measurements stay explicit (`None`) instead of
//! being reported as freshly measured zeroes.

use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System, UpdateKind, Users};

use super::pressure::{read_memory_pressure, MemoryPressure};

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
    /// `Some(0)` for both means swap is not in use.
    pub swap_used_bytes: Option<u64>,
    pub swap_total_bytes: Option<u64>,
    pub memory_pressure: Option<MemoryPressure>,
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
    swap_used: u64,
    swap_total: u64,
    pressure: Option<MemoryPressure>,
}

impl SystemUsage {
    fn from_readings(raw: SystemReadings) -> Self {
        let memory_ok =
            raw.memory_total > 0 && raw.memory_used.is_some_and(|used| used <= raw.memory_total);
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
            swap_used_bytes: swap_ok.then_some(raw.swap_used),
            swap_total_bytes: swap_ok.then_some(raw.swap_total),
            memory_pressure: raw.pressure,
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
        exe: raw.exe,
    }
}

/// `sysinfo` on macOS records a new process's CPU times on its second refresh
/// and reports a real value only from the third; earlier values read 0.
const REFRESHES_FOR_CPU: u32 = 3;

/// One pid as earlier refreshes saw it.
struct Seen {
    start: u64,
    /// Refreshes this process has been seen in.
    count: u32,
    exe: Option<PathBuf>,
    /// The name after an `exec` under the same pid and start time, which the
    /// shared `System` would otherwise keep from before it.
    name: Option<String>,
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

/// Keeps one `System` alive so later samples have measured CPU.
pub struct Sampler {
    system: System,
    users: Users,
    refreshes: u32,
    /// What earlier refreshes saw of each pid.
    seen: HashMap<u32, Seen>,
    current_uid: u32,
    system_cpu_samples: u32,
    last_system_cpu_sample: Option<Instant>,
}

impl Default for Sampler {
    fn default() -> Self {
        Self::new()
    }
}

impl Sampler {
    pub fn new() -> Self {
        Self {
            system: System::new(),
            users: Users::new_with_refreshed_list(),
            refreshes: 0,
            seen: HashMap::new(),
            system_cpu_samples: 0,
            last_system_cpu_sample: None,
            // SAFETY: getuid has no preconditions and cannot fail.
            current_uid: unsafe { libc::getuid() },
        }
    }

    pub fn current_uid(&self) -> u32 {
        self.current_uid
    }

    /// Refreshes once. A process has measured CPU from its third sample on.
    pub fn sample(&mut self) -> ProcessSnapshot {
        self.refresh_system_usage();
        self.system.refresh_processes_specifics(
            ProcessesToUpdate::All,
            true,
            ProcessRefreshKind::new()
                .with_cpu()
                .with_memory()
                .with_user(UpdateKind::OnlyIfNotSet)
                .with_exe(UpdateKind::Always),
        );
        self.refreshes = self.refreshes.saturating_add(1);
        self.track_seen();
        self.snapshot()
    }

    fn refresh_system_usage(&mut self) {
        self.system.refresh_memory();
        let now = Instant::now();
        let due = self.last_system_cpu_sample.is_none_or(|previous| {
            now.duration_since(previous) >= sysinfo::MINIMUM_CPU_UPDATE_INTERVAL
        });
        if due {
            self.system.refresh_cpu_usage();
            self.system_cpu_samples = self.system_cpu_samples.saturating_add(1);
            self.last_system_cpu_sample = Some(now);
        }
    }

    fn track_seen(&mut self) {
        let mut seen = HashMap::with_capacity(self.system.processes().len());
        for process in self.system.processes().values() {
            let pid = process.pid().as_u32();
            let start = process.start_time();
            let exe = process.exe().map(PathBuf::from);
            let entry = match self.seen.get(&pid) {
                Some(previous) if previous.start == start => Seen {
                    start,
                    count: previous.count + 1,
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
                },
            };
            seen.insert(pid, entry);
        }
        self.seen = seen;
    }

    /// For one-shot callers such as the CLI: refreshes until CPU is measured,
    /// waiting the minimum CPU interval between refreshes (about 0.4 s).
    pub fn sample_measured(&mut self) -> ProcessSnapshot {
        while self.refreshes + 1 < REFRESHES_FOR_CPU {
            self.sample();
            std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        }
        self.sample()
    }

    fn system_usage(&self) -> SystemUsage {
        let cpus = self.system.cpus();
        let load = System::load_average();
        SystemUsage::from_readings(SystemReadings {
            cpu: if cpus.is_empty() {
                f32::NAN
            } else {
                self.system.global_cpu_usage()
            },
            cpu_measured: self.system_cpu_samples >= 2,
            cores: cpus.iter().map(|cpu| cpu.cpu_usage()).collect(),
            brand: cpus
                .first()
                .map_or_else(String::new, |c| c.brand().to_string()),
            load: [load.one, load.five, load.fifteen],
            memory_total: self.system.total_memory(),
            memory_used: crate::sys::used_memory(&self.system),
            swap_used: self.system.used_swap(),
            swap_total: self.system.total_swap(),
            pressure: read_memory_pressure(),
        })
    }

    fn snapshot(&self) -> ProcessSnapshot {
        let processes = self
            .system
            .processes()
            .values()
            .map(|process| {
                let user = process
                    .user_id()
                    .and_then(|uid| self.users.get_user_by_id(uid))
                    .map(|user| user.name().to_string());
                let mut raw = raw_process(process, user);
                let seen = self.seen.get(&raw.pid);
                if let Some(name) = seen.and_then(|s| s.name.clone()) {
                    raw.name = name;
                }
                let count = seen.map_or(0, |s| s.count);
                let info = to_info(raw, count >= REFRESHES_FOR_CPU, self.current_uid);
                // Only a process `to_info` could read gets a footprint, so a missing memory
                // reading keeps meaning "unreadable" across the boundary.
                ProcessInfo {
                    footprint_bytes: info
                        .memory_bytes
                        .and_then(|_| super::footprint::phys_footprint(info.pid)),
                    ..info
                }
            })
            .collect();
        ProcessSnapshot {
            processes,
            sampled_at: unix_now(),
            cpu_measured: self.refreshes >= REFRESHES_FOR_CPU,
            system_usage: self.system_usage(),
        }
    }
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
    fn measured_sample_keeps_cpu_value() {
        let info = to_info(raw(4096, 501), true, 501);
        assert_eq!(info.cpu_percent, Some(12.5));
        assert!(info.cpu_measured);
    }
}
