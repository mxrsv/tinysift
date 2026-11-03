//! Whole-machine sampling that never walks processes. The menu bar app reads this every
//! few seconds while no window is open, so it holds only CPU, memory and network state.

use std::time::Instant;

use sysinfo::{MemoryRefreshKind, System};

use super::network::NetworkCounters;
use super::snapshot::{usage_of, SystemUsage};

/// CPU, memory, swap, pressure and network of the whole machine, without `process_list`'s walk
/// over every process. Keeps its own `System`, so it never disturbs the process sampler's
/// CPU intervals.
pub struct MachineSampler {
    system: System,
    cpu_samples: u32,
    last_cpu_sample: Option<Instant>,
    network: NetworkCounters,
}

impl Default for MachineSampler {
    fn default() -> Self {
        Self::new()
    }
}

impl MachineSampler {
    pub fn new() -> Self {
        Self {
            system: System::new(),
            cpu_samples: 0,
            last_cpu_sample: None,
            network: NetworkCounters::new(),
        }
    }

    /// Refreshes once. CPU and network are rates between two refreshes at least
    /// `MINIMUM_CPU_UPDATE_INTERVAL` apart, so the first reading reports them warming up.
    pub fn sample(&mut self) -> SystemUsage {
        self.system
            .refresh_memory_specifics(MemoryRefreshKind::new().with_ram().with_swap());
        let now = Instant::now();
        let due = self.last_cpu_sample.is_none_or(|previous| {
            now.duration_since(previous) >= sysinfo::MINIMUM_CPU_UPDATE_INTERVAL
        });
        if due {
            self.system.refresh_cpu_usage();
            self.cpu_samples = self.cpu_samples.saturating_add(1);
            self.last_cpu_sample = Some(now);
        }
        let network = self.network.sample();
        usage_of(&self.system, self.cpu_samples, network)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_sample_is_warming_up_and_the_next_is_measured() {
        let mut sampler = MachineSampler::new();
        let first = sampler.sample();
        assert!(first.cpu_warming_up && first.cpu_percent.is_none());
        assert!(first.network_warming_up && first.network.is_none());
        assert!(first.memory_total_bytes.is_some_and(|total| total > 0));
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        let next = sampler.sample();
        assert!(next
            .cpu_percent
            .is_some_and(|cpu| (0.0..=100.0).contains(&cpu)));
        assert!(!next.network_warming_up);
        assert_eq!(
            next.cpu_cores.len(),
            next.logical_cores.unwrap_or_default() as usize
        );
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn memory_split_sums_to_the_used_figure() {
        let usage = MachineSampler::new().sample();
        let parts = [
            usage.memory_apps_bytes,
            usage.memory_wired_bytes,
            usage.memory_compressed_bytes,
        ]
        .map(|part| part.expect("the split is readable"));
        assert_eq!(parts.iter().sum::<u64>(), usage.memory_used_bytes.unwrap());
    }
}
