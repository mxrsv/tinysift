//! Whole-machine network throughput: bytes received and sent per second over the Mac's
//! ports and AirDrop links, from the kernel's interface counters through `sysinfo`.
//! There is no per-process figure here; macOS has no public API for one.

use std::time::{Duration, Instant};

use sysinfo::Networks;

/// Interfaces whose bytes count: Ethernet, Wi-Fi and Thunderbolt ports (`en*`) and AirDrop
/// links (`awdl*`, `llw*`). Loopback, tunnels (`utun*`) and bridges stay out, so bytes that
/// also cross a port are not counted twice.
pub const COUNTED_INTERFACE_PREFIXES: [&str; 3] = ["en", "awdl", "llw"];

/// Two readings closer than this would divide a few bytes by a few milliseconds.
const MINIMUM_INTERVAL: Duration = sysinfo::MINIMUM_CPU_UPDATE_INTERVAL;

/// Bytes per second since the previous sample, summed over the counted interfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NetworkThroughput {
    pub received_bytes_per_second: u64,
    pub sent_bytes_per_second: u64,
}

/// One sample's result. `throughput` is `None` while the first sample sets the baseline
/// (`warming_up`) and when no counted interface could be read (not warming up: unavailable).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct NetworkReading {
    pub throughput: Option<NetworkThroughput>,
    pub warming_up: bool,
}

/// Keeps the previous counters, so each sample is a rate since the one before.
pub struct NetworkCounters {
    networks: Networks,
    last_sample: Option<Instant>,
    last: NetworkReading,
}

impl Default for NetworkCounters {
    fn default() -> Self {
        Self::new()
    }
}

impl NetworkCounters {
    pub fn new() -> Self {
        Self {
            networks: Networks::new(),
            last_sample: None,
            last: NetworkReading::default(),
        }
    }

    /// Reads the counters once. A sample sooner than `MINIMUM_INTERVAL` after the previous
    /// one repeats the previous reading instead of measuring a sliver.
    pub fn sample(&mut self) -> NetworkReading {
        let now = Instant::now();
        if let Some(previous) = self.last_sample {
            if now.duration_since(previous) < MINIMUM_INTERVAL {
                return self.last;
            }
        }
        self.networks.refresh_list();
        let elapsed = self
            .last_sample
            .map(|previous| now.duration_since(previous));
        self.last_sample = Some(now);
        let counted = self.networks.iter().filter(|(name, _)| counts(name)).fold(
            None,
            |sum: Option<(u64, u64)>, (_, data)| {
                let (received, sent) = sum.unwrap_or_default();
                Some((
                    received.saturating_add(data.received()),
                    sent.saturating_add(data.transmitted()),
                ))
            },
        );
        self.last = match (counted, elapsed) {
            (None, _) => NetworkReading::default(),
            (Some(_), None) => NetworkReading {
                throughput: None,
                warming_up: true,
            },
            (Some((received, sent)), Some(elapsed)) => NetworkReading {
                throughput: Some(throughput(received, sent, elapsed)),
                warming_up: false,
            },
        };
        self.last
    }
}

/// Whether an interface's bytes count toward the whole-machine figure.
pub fn counts(interface: &str) -> bool {
    COUNTED_INTERFACE_PREFIXES
        .iter()
        .any(|prefix| interface.starts_with(prefix))
}

fn throughput(received: u64, sent: u64, elapsed: Duration) -> NetworkThroughput {
    let per_second = |bytes: u64| (bytes as f64 / elapsed.as_secs_f64()).round() as u64;
    NetworkThroughput {
        received_bytes_per_second: per_second(received),
        sent_bytes_per_second: per_second(sent),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_and_airdrop_count_but_loopback_tunnels_and_bridges_do_not() {
        for name in ["en0", "en5", "awdl0", "llw0"] {
            assert!(counts(name), "{name}");
        }
        for name in [
            "lo0",
            "utun3",
            "bridge100",
            "gif0",
            "stf0",
            "ap1",
            "anpi0",
            "vmenet0",
        ] {
            assert!(!counts(name), "{name}");
        }
    }

    #[test]
    fn throughput_divides_bytes_by_the_elapsed_time() {
        let rate = throughput(4_000, 1_000, Duration::from_millis(2_000));
        assert_eq!(rate.received_bytes_per_second, 2_000);
        assert_eq!(rate.sent_bytes_per_second, 500);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn first_sample_is_warming_up_and_the_next_is_a_rate() {
        let mut counters = NetworkCounters::new();
        let first = counters.sample();
        assert!(first.warming_up && first.throughput.is_none());
        std::thread::sleep(MINIMUM_INTERVAL);
        let next = counters.sample();
        assert!(!next.warming_up);
        assert!(next.throughput.is_some(), "a Mac always has an en* port");
    }

    #[test]
    fn a_sample_too_soon_repeats_the_previous_reading() {
        let mut counters = NetworkCounters::new();
        let first = counters.sample();
        assert_eq!(counters.sample(), first);
    }
}
