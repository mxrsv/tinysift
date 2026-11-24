//! The startup APFS container's figures, read without a walk: `statfs` of every mounted volume of the container.
//!
//! Volumes of one container report the container's size and free space in `f_blocks` and `f_bavail`. `f_bfree` is the
//! container's free space too, so a volume's own used space comes from `getattrlist`'s `ATTR_VOL_SPACEUSED` (what `df` and
//! `diskutil apfs list` show). Volumes are matched to the container by the `diskN` of their device (`/dev/disk3s5` and the
//! sealed system snapshot `/dev/disk3s1s1` both belong to `disk3`).

/// One mounted volume of the container.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Volume {
    pub mount: String,
    pub used: u64,
}

/// The container that holds the user's data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Container {
    pub total: u64,
    pub available: u64,
    /// The Data volume (`/System/Volumes/Data`), which the breakdown walks.
    pub data: Volume,
    /// Files and folders the Data volume holds (`f_files - f_ffree`): the walk's progress total.
    pub data_files: u64,
    /// Every other mounted volume of the container.
    pub others: Vec<Volume>,
}

impl Container {
    pub(crate) fn used(&self) -> u64 {
        self.total.saturating_sub(self.available)
    }
}

pub(crate) const DATA_MOUNT: &str = "/System/Volumes/Data";
pub(crate) const VM_MOUNT: &str = "/System/Volumes/VM";

/// `disk3` of `/dev/disk3s5`; `None` for a device that is not a disk slice.
pub(crate) fn container_of(device: &str) -> Option<&str> {
    let name = device.strip_prefix("/dev/")?;
    let digits = name.strip_prefix("disk")?;
    let end = digits
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(digits.len());
    (end > 0).then(|| &name[..4 + end])
}

/// The volume's name in the breakdown: `System volume` for `/`, else its mount point's last part (`Preboot`, `Update`).
pub(crate) fn volume_name(mount: &str) -> String {
    if mount == "/" {
        return "System volume".into();
    }
    mount
        .rsplit('/')
        .next()
        .filter(|last| !last.is_empty())
        .unwrap_or(mount)
        .to_string()
}

#[cfg(target_os = "macos")]
pub(crate) fn startup_container() -> std::io::Result<Container> {
    use std::ffi::CStr;
    use std::io::{Error, ErrorKind};

    struct Mount {
        on: String,
        from: String,
        total: u64,
        available: u64,
        /// `None` when macOS would not say: never read as zero.
        used: Option<u64>,
        files: u64,
    }
    let mut buffer: *mut libc::statfs = std::ptr::null_mut();
    // SAFETY: getmntinfo points `buffer` at an array of `count` statfs records it owns; they are only read below,
    // before any other call that could replace that array on this thread.
    let count = unsafe { libc::getmntinfo(&mut buffer, libc::MNT_NOWAIT) };
    if count <= 0 || buffer.is_null() {
        return Err(Error::last_os_error());
    }
    // SAFETY: see above; `count` records are valid.
    let records = unsafe { std::slice::from_raw_parts(buffer, count as usize) };
    let text = |raw: &[libc::c_char]| {
        // SAFETY: the kernel NUL-terminates these fixed-size name fields.
        unsafe { CStr::from_ptr(raw.as_ptr()) }
            .to_string_lossy()
            .into_owned()
    };
    let mounts: Vec<Mount> = records
        .iter()
        .filter(|record| text(&record.f_fstypename) == "apfs")
        .map(|record| {
            let block = u64::from(record.f_bsize);
            Mount {
                on: text(&record.f_mntonname),
                from: text(&record.f_mntfromname),
                total: record.f_blocks.saturating_mul(block),
                available: record.f_bavail.saturating_mul(block),
                used: space_used(&record.f_mntonname),
                files: record.f_files.saturating_sub(record.f_ffree),
            }
        })
        .collect();
    let data = mounts
        .iter()
        .find(|mount| mount.on == DATA_MOUNT)
        .ok_or_else(|| {
            Error::new(
                ErrorKind::NotFound,
                "The startup disk's Data volume is not mounted.",
            )
        })?;
    // The Data volume's own figure decides the gap; without it nothing honest can be said. Another volume that does not answer
    // is left out, so its bytes land in `Recovery and other volumes` instead of vanishing.
    let data_used = data
        .used
        .ok_or_else(|| Error::other("macOS did not report the Data volume's used space."))?;
    let container = container_of(&data.from).map(str::to_owned);
    let others = mounts
        .iter()
        .filter(|mount| {
            mount.on != DATA_MOUNT
                && container.is_some()
                && container_of(&mount.from) == container.as_deref()
        })
        .filter_map(|mount| {
            mount.used.map(|used| Volume {
                mount: mount.on.clone(),
                used,
            })
        })
        .collect();
    Ok(Container {
        total: data.total,
        available: data.available,
        data: Volume {
            mount: data.on.clone(),
            used: data_used,
        },
        data_files: data.files,
        others,
    })
}

/// The bytes the volume mounted at `mount` uses on its own, from `ATTR_VOL_SPACEUSED`.
#[cfg(target_os = "macos")]
fn space_used(mount: &[libc::c_char]) -> Option<u64> {
    #[repr(C, align(8))]
    struct Reply {
        length: u32,
        used: [u8; 8],
        _spare: [u8; 4],
    }
    let mut request = libc::attrlist {
        bitmapcount: libc::ATTR_BIT_MAP_COUNT,
        reserved: 0,
        commonattr: 0,
        volattr: libc::ATTR_VOL_INFO | libc::ATTR_VOL_SPACEUSED,
        dirattr: 0,
        fileattr: 0,
        forkattr: 0,
    };
    let mut reply = Reply {
        length: 0,
        used: [0; 8],
        _spare: [0; 4],
    };
    // SAFETY: `mount` is the kernel's NUL-terminated mount path; `reply` outlives the call, which writes at most its size:
    // the length, then the one `off_t` requested (packed right after the length, 4-byte aligned).
    let rc = unsafe {
        libc::getattrlist(
            mount.as_ptr(),
            (&mut request as *mut libc::attrlist).cast(),
            (&mut reply as *mut Reply).cast(),
            std::mem::size_of::<Reply>(),
            0,
        )
    };
    let used = i64::from_ne_bytes(reply.used);
    (rc == 0 && reply.length as usize >= 12 && used >= 0).then_some(used as u64)
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn startup_container() -> std::io::Result<Container> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "The disk breakdown needs macOS.",
    ))
}
