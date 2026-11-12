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
    let (elapsed, contains_self) = result.expect("sample must finish while scan workers are held");
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
