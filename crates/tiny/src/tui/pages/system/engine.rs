//! What System reads, behind a trait so tests feed the mock's data.

use tiny_core::sys::DiskInfo;

/// One read of the machine, as the page draws it.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SystemReading {
    pub host: Option<String>,
    pub uptime_seconds: Option<u64>,
    /// The chip's name; empty when the engine could not read it.
    pub chip: String,
    pub cores: usize,
    /// `None` when the kernel refused the read.
    pub memory_used: Option<u64>,
    pub memory_total: u64,
    /// Each volume once (I19), the startup volume first.
    pub volumes: Vec<Volume>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Volume {
    pub name: String,
    pub used: u64,
    pub total: u64,
}

pub trait SystemEngine: Send + Sync + 'static {
    /// Blocks for the CPU sampling interval; runs on a worker thread.
    fn read(&self) -> SystemReading;
}

/// `tiny_core::sys::information`, the call `sift sys` makes.
pub struct RealSystem;

impl SystemEngine for RealSystem {
    fn read(&self) -> SystemReading {
        let info = tiny_core::sys::information();
        SystemReading {
            host: info.host,
            uptime_seconds: info.uptime_seconds,
            chip: info.cpu_model,
            cores: info.cpu_count,
            memory_used: info.memory_used,
            memory_total: info.memory_total,
            volumes: volumes_once(&info.disks),
        }
    }
}

/// Mounts of the startup volume's APFS container: the sealed system volume at
/// `/` and its sibling volumes under `/System/Volumes/` (Data, Preboot, VM…).
/// They share one capacity, so they are one row.
fn in_boot_container(mount_point: &str) -> bool {
    mount_point == "/" || mount_point.starts_with("/System/Volumes/")
}

/// Every volume once (I19). The boot container's mounts collapse into the one
/// volume the engine picks for the startup disk (`startup_capacity`: the Data
/// volume, else `/`); any other volume is keyed by its mount point. Samples
/// with no total, or more free than total, are left out as unreadable.
pub fn volumes_once(disks: &[DiskInfo]) -> Vec<Volume> {
    let startup = tiny_core::sys::startup_capacity(disks).map(|capacity| Volume {
        name: display_name(&capacity.name, &capacity.mount_point),
        used: capacity.total_bytes - capacity.available_bytes,
        total: capacity.total_bytes,
    });
    let mut seen: Vec<&str> = Vec::new();
    let others = disks
        .iter()
        .filter(|disk| !in_boot_container(&disk.mount_point))
        .filter(|disk| disk.total_bytes > 0 && disk.available_bytes <= disk.total_bytes)
        .filter(|disk| {
            let fresh = !seen.contains(&disk.mount_point.as_str());
            seen.push(&disk.mount_point);
            fresh
        })
        .map(|disk| Volume {
            name: display_name(&disk.name, &disk.mount_point),
            used: disk.total_bytes - disk.available_bytes,
            total: disk.total_bytes,
        })
        .collect::<Vec<_>>();
    startup.into_iter().chain(others).collect()
}

fn display_name(name: &str, mount_point: &str) -> String {
    if name.trim().is_empty() {
        mount_point.to_string()
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk(name: &str, mount: &str, total: u64, available: u64) -> DiskInfo {
        DiskInfo {
            name: name.into(),
            mount_point: mount.into(),
            total_bytes: total,
            available_bytes: available,
        }
    }

    #[test]
    fn the_startup_volume_is_listed_once_not_once_per_mount() {
        // What `sift sys` printed on the owner's Mac on 2026-11-23.
        let disks = [
            disk("Macintosh HD", "/", 494_384_795_648, 4_948_000_000),
            disk(
                "Macintosh HD",
                "/System/Volumes/Data",
                494_384_795_648,
                4_948_000_000,
            ),
            disk("AgentDock", "/Volumes/AgentDock", 24_473_000, 4_900_000),
            disk("Vitals", "/Volumes/Vitals", 96_490_000, 3_250_000),
        ];
        let volumes = volumes_once(&disks);
        let names: Vec<&str> = volumes.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["Macintosh HD", "AgentDock", "Vitals"]);
        assert_eq!(volumes[0].total, 494_384_795_648);
        assert_eq!(volumes[0].used, 494_384_795_648 - 4_948_000_000);
    }

    #[test]
    fn other_boot_container_mounts_and_repeated_mounts_are_dropped() {
        let disks = [
            disk("Macintosh HD", "/", 500, 60),
            disk("Data", "/System/Volumes/Data", 500, 60),
            disk("VM", "/System/Volumes/VM", 500, 60),
            disk("Backup", "/Volumes/Backup", 900, 100),
            disk("Backup", "/Volumes/Backup", 900, 100),
            disk("", "/Volumes/Untitled", 10, 5),
            disk("Broken", "/Volumes/Broken", 0, 0),
        ];
        let volumes = volumes_once(&disks);
        let names: Vec<&str> = volumes.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(names, ["Data", "Backup", "/Volumes/Untitled"]);
    }

    #[test]
    fn an_unreadable_startup_sample_is_left_out_not_shown_as_zero() {
        let disks = [
            disk("Macintosh HD", "/System/Volumes/Data", 0, 0),
            disk("Backup", "/Volumes/Backup", 900, 100),
        ];
        let volumes = volumes_once(&disks);
        assert_eq!(volumes.len(), 1);
        assert_eq!(volumes[0].name, "Backup");
    }
}
