//! What a helper process does for its app: a Chromium or Electron renderer, GPU or utility
//! process, a WebKit service, or a video decoder. Read for the few processes a detail panel
//! names, and cached per process identity, since a process never changes its role.
//!
//! Privacy: a command line can hold tokens, paths and URLs. [`parse_role`] is the only code that
//! sees the argument buffer; it looks at `--type`, `--utility-sub-type` and the presence of
//! `--extension-process`, returns a fieldless [`ProcessRole`], and the buffer is dropped in
//! [`read_args_role`]. No argument is stored, logged, cached or put in an error.

use std::collections::HashMap;

use super::snapshot::{ProcessInfo, ProcessKey};

/// The role of one helper process. Fieldless on purpose: it cannot carry any argument text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProcessRole {
    /// Chromium `--type=renderer`: a tab or a frame.
    Renderer,
    /// Chromium `--type=renderer --extension-process`.
    ExtensionRenderer,
    /// Chromium `--type=gpu-process`.
    Gpu,
    /// Chromium utility running `network.mojom.NetworkService`.
    Network,
    /// Chromium utility running `storage.mojom.StorageService`.
    Storage,
    /// Chromium utility running `audio.mojom.AudioService` or a video service.
    AudioVideo,
    /// Any other Chromium `--type` or utility sub-type.
    Utility,
    /// `com.apple.WebKit.WebContent` and its variants: a Safari or WebKit page.
    WebContent,
    /// `com.apple.WebKit.GPU`.
    WebGpu,
    /// `com.apple.WebKit.Networking`.
    WebNetworking,
    /// VideoToolbox's `VTDecoderXPCService`.
    VideoDecoder,
}

/// Roles already read, keyed by process identity so a reused PID is read again.
#[derive(Debug, Default)]
pub struct RoleCache {
    known: HashMap<ProcessKey, Option<ProcessRole>>,
}

impl RoleCache {
    /// The role of each process, in order. A process is read once per identity; identities not
    /// in `processes` are forgotten, so the cache never outgrows one request.
    pub fn roles(&mut self, processes: &[ProcessInfo]) -> Vec<Option<ProcessRole>> {
        self.roles_with(processes, classify)
    }

    fn roles_with(
        &mut self,
        processes: &[ProcessInfo],
        mut read: impl FnMut(&ProcessInfo) -> Option<ProcessRole>,
    ) -> Vec<Option<ProcessRole>> {
        let mut next = HashMap::with_capacity(processes.len());
        let roles = processes
            .iter()
            .map(|process| {
                let key = ProcessKey {
                    pid: process.pid,
                    start_time: process.start_time,
                };
                let role = match self.known.get(&key) {
                    Some(role) => *role,
                    None => read(process),
                };
                next.insert(key, role);
                role
            })
            .collect();
        self.known = next;
        roles
    }
}

/// The role of `process`: by executable name first, then by its arguments. `None` for another
/// user's process, an unreadable command line, or a process with no known role.
pub fn classify(process: &ProcessInfo) -> Option<ProcessRole> {
    if !process.is_current_user {
        return None;
    }
    let exe_name = process
        .exe
        .as_deref()
        .and_then(|path| path.file_name())
        .and_then(|name| name.to_str())
        .unwrap_or(&process.name);
    role_by_executable(exe_name).or_else(|| read_args_role(process.pid))
}

fn role_by_executable(name: &str) -> Option<ProcessRole> {
    match name {
        "com.apple.WebKit.GPU" => Some(ProcessRole::WebGpu),
        "com.apple.WebKit.Networking" => Some(ProcessRole::WebNetworking),
        "VTDecoderXPCService" => Some(ProcessRole::VideoDecoder),
        _ if name.starts_with("com.apple.WebKit.WebContent") => Some(ProcessRole::WebContent),
        _ => None,
    }
}

/// Reads `pid`'s `KERN_PROCARGS2` buffer, classifies it and drops it before returning.
#[cfg(target_os = "macos")]
fn read_args_role(pid: u32) -> Option<ProcessRole> {
    let pid = libc::c_int::try_from(pid).ok().filter(|pid| *pid > 0)?;
    let mut mib = [libc::CTL_KERN, libc::KERN_PROCARGS2, pid];
    let mut size: libc::size_t = 0;
    // SAFETY: a null output buffer asks only for the size, written to `size`.
    let status = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            std::ptr::null_mut(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 || size == 0 {
        return None;
    }
    let mut buffer = vec![0u8; size];
    // SAFETY: `buffer` is `size` writable bytes; the kernel writes at most `size` and updates it.
    let status = unsafe {
        libc::sysctl(
            mib.as_mut_ptr(),
            3,
            buffer.as_mut_ptr().cast(),
            &mut size,
            std::ptr::null_mut(),
            0,
        )
    };
    if status != 0 {
        return None;
    }
    parse_role(buffer.get(..size)?)
}

#[cfg(not(target_os = "macos"))]
fn read_args_role(_pid: u32) -> Option<ProcessRole> {
    None
}

/// The role in a `KERN_PROCARGS2` buffer: `argc` (native `i32`), the executable path, NUL
/// padding, then `argc` NUL-terminated arguments, then the environment, which is never read.
/// Only `--type`, `--utility-sub-type` and `--extension-process` are looked at.
fn parse_role(buffer: &[u8]) -> Option<ProcessRole> {
    let argc = i32::from_ne_bytes(buffer.get(..4)?.try_into().ok()?);
    let argc = usize::try_from(argc).ok()?;
    let rest = &buffer[4..];
    let path_end = rest.iter().position(|byte| *byte == 0)?;
    let rest = &rest[path_end..];
    let first_arg = rest.iter().position(|byte| *byte != 0)?;
    let mut kind = None;
    let mut sub_type = None;
    let mut extension = false;
    for arg in rest[first_arg..].split(|byte| *byte == 0).take(argc) {
        if let Some(value) = arg.strip_prefix(b"--type=") {
            kind = Some(value);
        } else if let Some(value) = arg.strip_prefix(b"--utility-sub-type=") {
            sub_type = Some(value);
        } else if arg == b"--extension-process" {
            extension = true;
        }
    }
    let role = match kind? {
        b"renderer" if extension => ProcessRole::ExtensionRenderer,
        b"renderer" => ProcessRole::Renderer,
        b"gpu-process" => ProcessRole::Gpu,
        b"utility" => utility_role(sub_type.unwrap_or_default()),
        _ => ProcessRole::Utility,
    };
    Some(role)
}

fn utility_role(sub_type: &[u8]) -> ProcessRole {
    let mentions_video = sub_type
        .windows(b"video".len())
        .any(|window| window.eq_ignore_ascii_case(b"video"));
    match sub_type {
        b"network.mojom.NetworkService" => ProcessRole::Network,
        b"storage.mojom.StorageService" => ProcessRole::Storage,
        b"audio.mojom.AudioService" => ProcessRole::AudioVideo,
        _ if mentions_video => ProcessRole::AudioVideo,
        _ => ProcessRole::Utility,
    }
}

#[cfg(test)]
mod tests;
