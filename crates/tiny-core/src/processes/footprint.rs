//! A process's physical footprint, read from `proc_pid_rusage`.

/// Dirty private memory plus its compressed and swapped pages, without clean file-backed or
/// shared library pages, as `top`'s MEM column reports. `None` when the read fails: macOS
/// denies it for another user's process, the process is gone, or the platform has no
/// `proc_pid_rusage`.
#[cfg(target_os = "macos")]
pub(crate) fn phys_footprint(pid: u32) -> Option<u64> {
    let pid = libc::c_int::try_from(pid).ok()?;
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
    (status == 0).then_some(info.ri_phys_footprint)
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn phys_footprint(_pid: u32) -> Option<u64> {
    None
}

#[cfg(all(test, target_os = "macos"))]
mod tests {
    use std::io::{BufRead, BufReader, Write};
    use std::os::fd::AsRawFd;
    use std::process::{Command, Stdio};

    use super::phys_footprint;
    use crate::processes::Sampler;

    const CHILD: &str = "TINY_FOOTPRINT_TEST_CHILD";
    const READY: &str = "footprint-test-ready";
    const MIB: u64 = 1 << 20;
    const LEN: usize = 64 << 20;

    #[test]
    fn own_process_is_readable_and_another_users_is_not() {
        assert!(phys_footprint(std::process::id()).is_some_and(|bytes| bytes > 0));
        // launchd (pid 1) belongs to root, so only a root test run may read it.
        let root = Sampler::new().current_uid() == 0;
        assert_eq!(phys_footprint(1).is_some(), root);
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
        let footprint = snapshot
            .processes
            .iter()
            .find(|p| p.pid == child.id())
            .and_then(|p| p.footprint_bytes)
            .expect("own child is sampled and readable");
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
}
