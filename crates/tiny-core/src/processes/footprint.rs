//! A process's physical footprint and cumulative CPU time, read from one `proc_pid_rusage`
//! call.

/// What one `proc_pid_rusage` call reads. Both fields are `None` when the read fails: macOS
/// denies it for another user's process, the process is gone, or the platform has no
/// `proc_pid_rusage`. An exited process that is not reaped yet reads a zero footprint, so it
/// reads `None` too: a live process always holds some dirty memory.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Usage {
    /// Dirty private memory plus its compressed and swapped pages, without clean file-backed
    /// or shared library pages, as `top`'s MEM column reports.
    pub footprint_bytes: Option<u64>,
    /// User plus system CPU time since the process started, including the time of the
    /// children it has reaped, in nanoseconds. A build or test run a session started and
    /// waited for still counts once it has finished.
    pub cpu_time_ns: Option<u64>,
}

#[cfg(target_os = "macos")]
pub(crate) fn read(pid: u32) -> Usage {
    let Ok(pid) = libc::c_int::try_from(pid) else {
        return Usage::default();
    };
    // SAFETY: an all-zero `rusage_info_v2` is a valid value of plain integers.
    let mut info: libc::rusage_info_v2 = unsafe { std::mem::zeroed() };
    // SAFETY: `info` is the structure the RUSAGE_INFO_V2 flavor fills.
    let status = unsafe {
        libc::proc_pid_rusage(
            pid,
            libc::RUSAGE_INFO_V2,
            (&mut info as *mut libc::rusage_info_v2).cast(),
        )
    };
    if status != 0 || info.ri_phys_footprint == 0 {
        return Usage::default();
    }
    // The kernel keeps all four in Mach absolute-time units, not nanoseconds.
    let ticks = info
        .ri_user_time
        .saturating_add(info.ri_system_time)
        .saturating_add(info.ri_child_user_time)
        .saturating_add(info.ri_child_system_time);
    Usage {
        footprint_bytes: Some(info.ri_phys_footprint),
        cpu_time_ns: timebase().map(|(numer, denom)| ticks_to_ns(ticks, numer, denom)),
    }
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn read(_pid: u32) -> Usage {
    Usage::default()
}

/// The length of one Mach absolute-time unit as `numer / denom` nanoseconds: 1/1 on Intel,
/// 125/3 on Apple silicon. Read once; `None` if the kernel will not say.
// libc deprecates its Mach items in favour of the `mach2` crate; the call is the same.
#[allow(deprecated)]
#[cfg(target_os = "macos")]
fn timebase() -> Option<(u32, u32)> {
    static TIMEBASE: std::sync::OnceLock<Option<(u32, u32)>> = std::sync::OnceLock::new();
    *TIMEBASE.get_or_init(|| {
        let mut info = libc::mach_timebase_info_data_t { numer: 0, denom: 0 };
        // SAFETY: `info` outlives the call, which only fills it.
        let status = unsafe { libc::mach_timebase_info(&mut info) };
        (status == libc::KERN_SUCCESS && info.numer != 0 && info.denom != 0)
            .then_some((info.numer, info.denom))
    })
}

/// Mach absolute-time units to nanoseconds, saturating instead of wrapping. `denom` is never
/// zero: `timebase` refuses one.
#[cfg(any(target_os = "macos", test))]
fn ticks_to_ns(ticks: u64, numer: u32, denom: u32) -> u64 {
    let ns = u128::from(ticks) * u128::from(numer) / u128::from(denom);
    u64::try_from(ns).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod unit_tests {
    use super::ticks_to_ns;

    #[test]
    fn intel_ticks_are_nanoseconds() {
        assert_eq!(ticks_to_ns(1_234_567, 1, 1), 1_234_567);
    }

    #[test]
    fn apple_silicon_ticks_are_converted_not_read_as_nanoseconds() {
        // 24 MHz: one second is 24 million ticks of 125/3 ns each.
        assert_eq!(ticks_to_ns(24_000_000, 125, 3), 1_000_000_000);
    }

    #[test]
    fn a_huge_reading_saturates_instead_of_wrapping() {
        assert_eq!(ticks_to_ns(u64::MAX, 125, 3), u64::MAX);
        assert_eq!(ticks_to_ns(u64::MAX, 1, 1), u64::MAX);
    }
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::fd::AsRawFd;
    use std::process::{Child, Command, Stdio};
    use std::time::{Duration, Instant};

    use super::{read, Usage};
    use crate::processes::Sampler;

    const CHILD: &str = "TINY_FOOTPRINT_TEST_CHILD";
    const READY: &str = "footprint-test-ready";
    const MIB: u64 = 1 << 20;
    const LEN: usize = 64 << 20;

    /// Role of a re-run test binary in the CPU time tests.
    const CPU_CHILD: &str = "TINY_CPU_TIME_TEST_CHILD";
    const HALF: &str = "cpu-time-test-half";
    const BURN: Duration = Duration::from_millis(500);
    const MS: u64 = 1_000_000;

    #[test]
    fn own_process_is_readable_and_another_users_is_not() {
        let me = read(std::process::id());
        assert!(me.footprint_bytes.is_some_and(|bytes| bytes > 0));
        assert!(me.cpu_time_ns.is_some_and(|ns| ns > 0));
        // launchd (pid 1) belongs to root, so only a root test run may read it.
        let root = Sampler::new().current_uid() == 0;
        let launchd = read(1);
        assert_eq!(launchd.footprint_bytes.is_some(), root);
        assert_eq!(launchd.cpu_time_ns.is_some(), root);
    }

    #[test]
    fn exited_child_not_yet_reaped_is_unreadable_not_zero() {
        let mut child = Command::new("/usr/bin/true").spawn().unwrap();
        let pid = child.id();
        // SAFETY: an all-zero `siginfo_t` is a valid value to fill.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // WNOWAIT returns once the child has exited and leaves it unreaped.
        // SAFETY: `info` outlives the call.
        let status =
            unsafe { libc::waitid(libc::P_PID, pid, &mut info, libc::WEXITED | libc::WNOWAIT) };
        let usage = read(pid);
        child.wait().unwrap();
        assert_eq!(status, 0);
        assert_eq!(usage, Usage::default());
    }

    /// A child test process holds the memory, so other tests' allocations cannot move the
    /// reading. Sampling it through `Sampler` covers the snapshot's wiring too.
    #[test]
    fn footprint_counts_dirty_memory_but_not_a_mapped_file() {
        if std::env::var_os(CHILD).is_some() {
            hold_dirty_memory_and_a_mapped_file();
            return;
        }
        let mut child = Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "processes::footprint::tests::footprint_counts_dirty_memory_but_not_a_mapped_file",
                "--nocapture",
            ])
            .env(CHILD, "1")
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        // libtest can print the marker mid-line, after the test's name.
        let ready = BufReader::new(child.stdout.take().unwrap())
            .lines()
            .map_while(Result::ok)
            .any(|line| line.contains(READY));
        let snapshot = Sampler::new().sample();
        // Stop the child before any assertion can fail.
        let _ = child.kill();
        child.wait().unwrap();
        assert!(ready, "the child must reach the mapped state");
        let sampled = snapshot
            .processes
            .iter()
            .find(|p| p.pid == child.id())
            .expect("own child is sampled");
        let footprint = sampled.footprint_bytes.expect("own child is readable");
        assert!(sampled.cpu_time_ns.is_some_and(|ns| ns > 0));
        // 64 MiB of dirty memory counts; the 64 MiB mapped file would push it past 128 MiB.
        assert!(
            (64 * MIB..112 * MIB).contains(&footprint),
            "footprint {} MiB",
            footprint / MIB
        );
    }

    fn hold_dirty_memory_and_a_mapped_file() {
        // Varied bytes, so no page is a single-value page the compressor stores specially.
        let mut state = 0x9E37_79B9_7F4A_7C15_u64;
        let data: Vec<u8> = (0..LEN)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                state as u8
            })
            .collect();
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(&data).unwrap();
        // SAFETY: a fresh read-only private mapping of a file this process just wrote.
        let map = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                LEN,
                libc::PROT_READ,
                libc::MAP_PRIVATE,
                file.as_raw_fd(),
                0,
            )
        };
        assert_ne!(map, libc::MAP_FAILED);
        // 4 KiB steps reach every page whatever the page size.
        let touched: u64 = (0..LEN)
            .step_by(4096)
            // SAFETY: every offset lies inside the LEN-byte mapping.
            .map(|offset| u64::from(unsafe { *map.cast::<u8>().add(offset) }))
            .sum();
        std::hint::black_box((&data, touched));
        println!("{READY}");
        // The parent kills this process once it has sampled it.
        std::thread::sleep(std::time::Duration::from_secs(60));
    }

    /// The busy work runs in a child, so other tests' threads cannot move the reading.
    /// `ps` and the child's own `getrusage` convert the kernel's counters themselves, so a
    /// units slip (Mach ticks read as nanoseconds, 41 times too little on Apple silicon)
    /// fails here.
    #[test]
    fn busy_child_cpu_time_grows_and_matches_ps() {
        if std::env::var_os(CPU_CHILD).is_some() {
            burn(BURN);
            println!("{HALF}");
            burn(BURN);
            report_and_wait(0);
            return;
        }
        let mut child = rerun("busy_child_cpu_time_grows_and_matches_ps", "busy");
        let mut lines = BufReader::new(child.stdout.take().unwrap())
            .lines()
            .map_while(Result::ok);
        let half = lines
            .any(|line| line.contains(HALF))
            .then(|| read(child.id()).cpu_time_ns);
        let reported = reported(&mut lines);
        let done = read(child.id()).cpu_time_ns;
        let ps = ps_time_ns(child.id());
        // Stop the child before any assertion can fail.
        let _ = child.kill();
        child.wait().unwrap();
        let (half, done) = (half.flatten().unwrap(), done.unwrap());
        let (reported, _) = reported.expect("the child must report its time");
        assert!(done > half, "{half} ns then {done} ns");
        assert!(
            close(done, reported),
            "read {done} ns, getrusage {reported} ns"
        );
        assert!(close(done, ps), "read {done} ns, ps {ps} ns");
    }

    /// Open question 7 of the stats pages spec: a session's finished builds still count.
    #[test]
    fn reaped_children_count_in_cpu_time() {
        match std::env::var(CPU_CHILD).as_deref() {
            Ok("grandchild") => {
                burn(BURN);
                return;
            }
            Ok(_) => {
                let status = Command::new(std::env::current_exe().unwrap())
                    .args(["--exact", &test_path("reaped_children_count_in_cpu_time")])
                    .env(CPU_CHILD, "grandchild")
                    .stdout(Stdio::null())
                    .status()
                    .unwrap();
                assert!(status.success());
                report_and_wait(libc::RUSAGE_CHILDREN);
                return;
            }
            Err(_) => {}
        }
        let mut child = rerun("reaped_children_count_in_cpu_time", "parent");
        let mut lines = BufReader::new(child.stdout.take().unwrap())
            .lines()
            .map_while(Result::ok);
        let reported = reported(&mut lines);
        let read_ns = read(child.id()).cpu_time_ns;
        let _ = child.kill();
        child.wait().unwrap();
        let (total, children) = reported.expect("the child must report its time");
        let read_ns = read_ns.unwrap();
        // The child itself barely runs, so without its children's time the reading would
        // miss most of the total.
        assert!(children > 100 * MS, "the grandchild ran {children} ns");
        assert!(
            close(read_ns, total),
            "read {read_ns} ns, getrusage {total} ns"
        );
    }

    fn test_path(name: &str) -> String {
        format!("processes::footprint::tests::{name}")
    }

    fn rerun(test: &str, role: &str) -> Child {
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &test_path(test), "--nocapture"])
            .env(CPU_CHILD, role)
            .stdout(Stdio::piped())
            .spawn()
            .unwrap()
    }

    fn burn(time: Duration) {
        let start = Instant::now();
        let mut state = 1_u64;
        while start.elapsed() < time {
            state = std::hint::black_box(state.wrapping_mul(6_364_136_223_846_793_005) + 1);
        }
    }

    /// Prints this process's CPU time, plus that of `who` when it names the reaped
    /// children, then idles so the parent reads a stable figure.
    fn report_and_wait(who: libc::c_int) {
        let own = rusage_ns(libc::RUSAGE_SELF);
        let children = if who == libc::RUSAGE_CHILDREN {
            rusage_ns(who)
        } else {
            0
        };
        println!("{READY} {} {children}", own + children);
        // The parent kills this process once it has read it.
        std::thread::sleep(Duration::from_secs(60));
    }

    /// The `(total, children)` nanoseconds a child printed after `READY`.
    fn reported(lines: &mut impl Iterator<Item = String>) -> Option<(u64, u64)> {
        let line = lines.find(|line| line.contains(READY))?;
        let mut numbers = line[line.find(READY)? + READY.len()..]
            .split_whitespace()
            .map(|n| n.parse::<u64>().ok());
        Some((numbers.next()??, numbers.next()??))
    }

    fn rusage_ns(who: libc::c_int) -> u64 {
        // SAFETY: an all-zero `rusage` is a valid value to fill.
        let mut usage: libc::rusage = unsafe { std::mem::zeroed() };
        // SAFETY: `usage` outlives the call.
        assert_eq!(unsafe { libc::getrusage(who, &mut usage) }, 0);
        let ns = |time: libc::timeval| {
            u64::try_from(time.tv_sec).unwrap() * 1_000_000_000
                + u64::try_from(time.tv_usec).unwrap() * 1_000
        };
        ns(usage.ru_utime) + ns(usage.ru_stime)
    }

    fn ps_time_ns(pid: u32) -> u64 {
        let out = Command::new("/bin/ps")
            .args(["-o", "time=", "-p", &pid.to_string()])
            .output()
            .unwrap();
        let text = String::from_utf8(out.stdout).unwrap();
        parse_ps_time(text.trim()).unwrap_or_else(|| panic!("ps time {text:?}"))
    }

    /// `ps`'s `[dd-][hh:]mm:ss.cc` in nanoseconds.
    fn parse_ps_time(text: &str) -> Option<u64> {
        let (days, clock) = match text.split_once('-') {
            Some((days, clock)) => (days.parse::<f64>().ok()?, clock),
            None => (0.0, text),
        };
        let seconds = clock.split(':').try_fold(0.0, |acc, part| {
            Some(acc * 60.0 + part.parse::<f64>().ok()?)
        })?;
        Some(((days * 86_400.0 + seconds) * 1e9).round() as u64)
    }

    #[test]
    fn ps_time_parses_every_width() {
        assert_eq!(parse_ps_time("0:01.25"), Some(1_250 * MS));
        assert_eq!(parse_ps_time("1:02:03.00"), Some(3_723_000 * MS));
        assert_eq!(parse_ps_time("1-00:00:01.00"), Some(86_401_000 * MS));
        assert_eq!(parse_ps_time(""), None);
    }

    /// Within 20 ms and 5%: `ps` prints centiseconds, and the child does a little work
    /// between reporting and idling.
    fn close(read: u64, expected: u64) -> bool {
        read.abs_diff(expected) <= 20 * MS + expected / 20
    }
}
