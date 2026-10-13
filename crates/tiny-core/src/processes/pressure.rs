//! macOS memory pressure, read from the kernel's `kern.memorystatus_vm_pressure_level`.

/// The kernel's own summary of how hard the machine is working to free memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemoryPressure {
    Normal,
    Warning,
    Critical,
}

impl MemoryPressure {
    /// Maps the sysctl level (1, 2, 4); any other value is unavailable.
    pub fn from_level(level: i32) -> Option<Self> {
        match level {
            1 => Some(Self::Normal),
            2 => Some(Self::Warning),
            4 => Some(Self::Critical),
            _ => None,
        }
    }
}

/// Current pressure; `None` when the sysctl fails or returns an unknown level.
#[cfg(target_os = "macos")]
pub fn read_memory_pressure() -> Option<MemoryPressure> {
    let mut level: libc::c_int = 0;
    let mut size = std::mem::size_of::<libc::c_int>();
    // SAFETY: the name is NUL-terminated and `level`/`size` are valid for the
    // sizes passed; no new value is set.
    let status = unsafe {
        libc::sysctlbyname(
            c"kern.memorystatus_vm_pressure_level".as_ptr(),
            (&mut level as *mut libc::c_int).cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    (status == 0 && size == std::mem::size_of::<libc::c_int>())
        .then(|| MemoryPressure::from_level(level))
        .flatten()
}

#[cfg(not(target_os = "macos"))]
pub fn read_memory_pressure() -> Option<MemoryPressure> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_map_and_unknown_values_are_unavailable() {
        assert_eq!(MemoryPressure::from_level(1), Some(MemoryPressure::Normal));
        assert_eq!(MemoryPressure::from_level(2), Some(MemoryPressure::Warning));
        assert_eq!(
            MemoryPressure::from_level(4),
            Some(MemoryPressure::Critical)
        );
        for unknown in [-1, 0, 3, 5, i32::MAX] {
            assert_eq!(MemoryPressure::from_level(unknown), None);
        }
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn live_reader_returns_a_level_on_macos() {
        assert!(read_memory_pressure().is_some());
    }
}
