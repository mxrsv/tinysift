use serde::{Deserialize, Serialize};
use sysinfo::{Disks, System};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskInfo {
    pub name: String,
    pub mount_point: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemInfo {
    pub os: Option<String>,
    pub host: Option<String>,
    pub uptime_seconds: Option<u64>,
    pub cpu_count: usize,
    pub cpu_model: String,
    pub cpu_usage: f32,
    pub memory_total: u64,
    /// `None` when the kernel refuses the read.
    pub memory_used: Option<u64>,
    pub disks: Vec<DiskInfo>,
}

/// macOS user data lives on this volume; `/` is the sealed system volume of the same APFS container.
const DATA_VOLUME: &str = "/System/Volumes/Data";
const ROOT_VOLUME: &str = "/";

/// Capacity of one volume. Built only from a sample that reports a usable total.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiskCapacity {
    pub name: String,
    pub mount_point: String,
    pub total_bytes: u64,
    pub available_bytes: u64,
}

/// Disk-only sample: no CPU sampling and no sleep, so it is cheap to repeat.
pub fn disks() -> Vec<DiskInfo> {
    Disks::new_with_refreshed_list()
        .iter()
        .map(|disk| DiskInfo {
            name: disk.name().to_string_lossy().into_owned(),
            mount_point: disk.mount_point().display().to_string(),
            total_bytes: disk.total_space(),
            available_bytes: disk.available_space(),
        })
        .collect()
}

/// The volume holding the user's data, or `None` when no sample is trustworthy.
/// Volumes of one APFS container report the same shared capacity, so exactly one
/// is returned and they are never summed. Zero or inconsistent samples are unavailable.
pub fn startup_capacity(disks: &[DiskInfo]) -> Option<DiskCapacity> {
    [DATA_VOLUME, ROOT_VOLUME]
        .iter()
        .find_map(|mount| disks.iter().find(|disk| disk.mount_point == *mount))
        .filter(|disk| disk.total_bytes > 0 && disk.available_bytes <= disk.total_bytes)
        .map(|disk| DiskCapacity {
            name: disk.name.clone(),
            mount_point: disk.mount_point.clone(),
            total_bytes: disk.total_bytes,
            available_bytes: disk.available_bytes,
        })
}

/// Used pages: app memory (anonymous pages less purgeable ones), wired memory and the
/// compressor's pages, the three parts Activity Monitor lists. File cache is not used memory.
#[cfg(any(target_os = "macos", test))]
fn used_pages(internal: u64, purgeable: u64, wired: u64, compressor: u64) -> u64 {
    internal
        .saturating_sub(purgeable)
        .saturating_add(wired)
        .saturating_add(compressor)
}

/// Memory in use as app, wired and compressed memory. `sysinfo`'s macOS figure is
/// active + wired + compressed + speculative, which misses inactive app memory and
/// counts active file cache, so it can read gigabytes apart. `None` when the kernel
/// refuses the read.
#[cfg(target_os = "macos")]
pub fn used_memory(_system: &System) -> Option<u64> {
    use std::sync::OnceLock;
    // One send right for the process's lifetime instead of a new reference per sample.
    static HOST: OnceLock<libc::mach_port_t> = OnceLock::new();
    // `libc` points to the `mach2` crate; one call does not justify the dependency.
    #[allow(deprecated)]
    // SAFETY: takes no arguments and returns this host's port.
    let host = *HOST.get_or_init(|| unsafe { libc::mach_host_self() });
    // SAFETY: an all-zero `vm_statistics64` is a valid value of plain integers.
    let mut stats: libc::vm_statistics64 = unsafe { std::mem::zeroed() };
    let mut count = libc::HOST_VM_INFO64_COUNT;
    // SAFETY: `stats` is the HOST_VM_INFO64 structure and `count` is its size in integers.
    let status = unsafe {
        libc::host_statistics64(
            host,
            libc::HOST_VM_INFO64,
            (&mut stats as *mut libc::vm_statistics64).cast(),
            &mut count,
        )
    };
    // SAFETY: reads a constant system value.
    let page = u64::try_from(unsafe { libc::sysconf(libc::_SC_PAGESIZE) }).ok()?;
    (status == libc::KERN_SUCCESS && page > 0).then(|| {
        used_pages(
            stats.internal_page_count.into(),
            stats.purgeable_count.into(),
            stats.wire_count.into(),
            stats.compressor_page_count.into(),
        )
        .saturating_mul(page)
    })
}

#[cfg(not(target_os = "macos"))]
pub fn used_memory(system: &System) -> Option<u64> {
    Some(system.used_memory())
}

pub fn information() -> SystemInfo {
    let mut system = System::new_all();
    std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
    system.refresh_cpu_usage();
    let uptime = System::uptime();
    SystemInfo {
        os: System::name(),
        host: System::host_name(),
        uptime_seconds: (uptime <= 10 * 365 * 86_400).then_some(uptime),
        cpu_count: system.cpus().len(),
        cpu_model: system
            .cpus()
            .first()
            .map(|cpu| cpu.brand().trim().to_owned())
            .unwrap_or_default(),
        cpu_usage: system.global_cpu_usage(),
        memory_total: system.total_memory(),
        memory_used: used_memory(&system),
        disks: disks(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn disk(mount: &str, total: u64, available: u64) -> DiskInfo {
        DiskInfo {
            name: mount.into(),
            mount_point: mount.into(),
            total_bytes: total,
            available_bytes: available,
        }
    }

    #[test]
    fn data_volume_is_chosen_once_over_the_shared_system_volume() {
        let capacity = startup_capacity(&[
            disk("/", 500, 60),
            disk(DATA_VOLUME, 500, 60),
            disk("/x", 9, 9),
        ])
        .unwrap();
        assert_eq!(capacity.mount_point, DATA_VOLUME);
        assert_eq!((capacity.total_bytes, capacity.available_bytes), (500, 60));
    }

    #[test]
    fn root_is_used_without_a_data_volume() {
        let capacity = startup_capacity(&[disk("/mnt", 9, 9), disk("/", 500, 60)]).unwrap();
        assert_eq!(capacity.mount_point, "/");
    }

    #[test]
    fn missing_or_invalid_samples_are_unavailable_not_zero() {
        assert!(startup_capacity(&[]).is_none());
        assert!(startup_capacity(&[disk("/mnt", 9, 9)]).is_none());
        assert!(startup_capacity(&[disk(DATA_VOLUME, 0, 0)]).is_none());
        assert!(startup_capacity(&[disk(DATA_VOLUME, 100, 101)]).is_none());
    }

    #[test]
    fn used_memory_is_app_wired_and_compressed_not_active_pages() {
        // Inactive app memory counts and file cache does not, whatever the queue.
        assert_eq!(used_pages(500, 20, 300, 400), 1180);
        assert_eq!(used_pages(10, 20, 300, 400), 700);
        assert_eq!(used_pages(u64::MAX, 0, 1, 0), u64::MAX);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn live_used_memory_fits_in_physical_memory() {
        let system = System::new_with_specifics(
            sysinfo::RefreshKind::new().with_memory(sysinfo::MemoryRefreshKind::new().with_ram()),
        );
        let used = used_memory(&system).expect("host_statistics64 succeeds");
        assert!(used > 0 && used <= system.total_memory());
    }

    #[test]
    fn invalid_data_volume_does_not_fall_back_to_a_different_volume() {
        assert!(startup_capacity(&[disk(DATA_VOLUME, 0, 0), disk("/", 500, 60)]).is_none());
    }
}
