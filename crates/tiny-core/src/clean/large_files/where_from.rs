//! "Downloaded from": the site a browser or Mail recorded for a file in its
//! `com.apple.metadata:kMDItemWhereFroms` extended attribute (LV-L6, a
//! checked fact). The attribute is a binary property list holding an array
//! of strings, the download URL first. Only the host leaves this module: a
//! URL can carry a signed token, and the row needs no more than the site.

use std::path::Path;

/// The attribute Safari, Chrome, Firefox, Mail and AirDrop write.
const WHERE_FROMS: &str = "com.apple.metadata:kMDItemWhereFroms";
/// A larger value is not a list of a few URLs; it is ignored.
const MAX_VALUE_BYTES: usize = 64 * 1024;
/// Strings read from the array at most.
const MAX_STRINGS: usize = 8;

/// The host of the first web or FTP URL in the file's where-from list
/// (`github.com`, a leading `www.` dropped), or `None` when the attribute is
/// missing, unreadable or names no such URL (an AirDrop device name, a Mail
/// message). Never follows a symlink.
pub fn downloaded_from(path: &Path) -> Option<String> {
    let value = read_attribute(path, WHERE_FROMS)?;
    binary_plist_strings(&value)?
        .iter()
        .find_map(|text| web_host(text))
}

#[cfg(target_os = "macos")]
fn read_attribute(path: &Path, name: &str) -> Option<Vec<u8>> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let path = CString::new(path.as_os_str().as_bytes()).ok()?;
    let name = CString::new(name).ok()?;
    let mut buffer = vec![0u8; MAX_VALUE_BYTES];
    // SAFETY: both strings are NUL-terminated and outlive the call; the
    // buffer is writable for its full length, which is the size passed.
    let read = unsafe {
        libc::getxattr(
            path.as_ptr(),
            name.as_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            0,
            libc::XATTR_NOFOLLOW,
        )
    };
    let read = usize::try_from(read).ok()?;
    buffer.truncate(read);
    Some(buffer)
}

#[cfg(not(target_os = "macos"))]
fn read_attribute(_path: &Path, _name: &str) -> Option<Vec<u8>> {
    None
}

/// `https://www.github.com/x` → `github.com`; anything but http(s) or ftp
/// is `None`.
pub(crate) fn web_host(text: &str) -> Option<String> {
    let (scheme, rest) = text.split_once("://")?;
    if !["http", "https", "ftp"].contains(&scheme.to_ascii_lowercase().as_str()) {
        return None;
    }
    let authority = rest.split(['/', '?', '#']).next()?;
    let host_port = authority.rsplit('@').next()?;
    let host = match host_port.strip_prefix('[') {
        // An IPv6 literal keeps its brackets' content.
        Some(v6) => v6.split(']').next()?,
        None => host_port.split(':').next()?,
    };
    let host = host.to_ascii_lowercase();
    let host = host.strip_prefix("www.").unwrap_or(&host);
    let valid = !host.is_empty()
        && host
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':'));
    valid.then(|| host.to_string())
}

/// The strings of a binary property list whose top object is an array of
/// strings (ASCII or UTF-16); `None` for anything else. Bounds-checked
/// throughout: a malformed value reads as `None`, never panics.
pub(crate) fn binary_plist_strings(data: &[u8]) -> Option<Vec<String>> {
    if data.len() < 8 + 32 || &data[..8] != b"bplist00" {
        return None;
    }
    let trailer = &data[data.len() - 32..];
    let offset_size = usize::from(trailer[6]);
    let ref_size = usize::from(trailer[7]);
    let count = be_uint(&trailer[8..16])?;
    let top = be_uint(&trailer[16..24])?;
    let table = be_uint(&trailer[24..32])?;
    let offset_of = |index: u64| -> Option<usize> {
        if index >= count {
            return None;
        }
        let at = table.checked_add(index.checked_mul(offset_size as u64)?)?;
        let at = usize::try_from(at).ok()?;
        let offset = be_uint(data.get(at..at.checked_add(offset_size)?)?)?;
        usize::try_from(offset).ok()
    };
    let array_at = offset_of(top)?;
    let marker = *data.get(array_at)?;
    if marker >> 4 != 0xA {
        return None;
    }
    let (length, refs_at) = object_length(data, array_at)?;
    let mut strings = Vec::new();
    for index in 0..length.min(MAX_STRINGS) {
        let at = refs_at.checked_add(index.checked_mul(ref_size)?)?;
        let reference = be_uint(data.get(at..at.checked_add(ref_size)?)?)?;
        if let Some(text) = string_at(data, offset_of(reference)?) {
            strings.push(text);
        }
    }
    Some(strings)
}

/// A string object (ASCII `0x5_` or UTF-16 big-endian `0x6_`) at `at`.
fn string_at(data: &[u8], at: usize) -> Option<String> {
    let marker = *data.get(at)?;
    let (length, start) = object_length(data, at)?;
    match marker >> 4 {
        0x5 => {
            let bytes = data.get(start..start.checked_add(length)?)?;
            bytes
                .is_ascii()
                .then(|| String::from_utf8_lossy(bytes).into_owned())
        }
        0x6 => {
            let bytes = data.get(start..start.checked_add(length.checked_mul(2)?)?)?;
            let units: Vec<u16> = bytes
                .as_chunks::<2>()
                .0
                .iter()
                .map(|pair| u16::from_be_bytes(*pair))
                .collect();
            String::from_utf16(&units).ok()
        }
        _ => None,
    }
}

/// An object's length and where its content starts: the low nibble, or a
/// following integer object when the nibble is `0xF`.
fn object_length(data: &[u8], at: usize) -> Option<(usize, usize)> {
    let nibble = data.get(at)? & 0x0F;
    if nibble != 0x0F {
        return Some((usize::from(nibble), at + 1));
    }
    let int_marker = *data.get(at + 1)?;
    if int_marker >> 4 != 0x1 {
        return None;
    }
    let width = 1usize << (int_marker & 0x0F);
    if width > 8 {
        return None;
    }
    let value = be_uint(data.get(at + 2..at + 2 + width)?)?;
    Some((usize::try_from(value).ok()?, at + 2 + width))
}

fn be_uint(bytes: &[u8]) -> Option<u64> {
    if bytes.is_empty() || bytes.len() > 8 {
        return None;
    }
    Some(bytes.iter().fold(0u64, |n, b| (n << 8) | u64::from(*b)))
}
