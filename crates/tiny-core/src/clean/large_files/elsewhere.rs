//! Group 5 of Large Files, "Remove these elsewhere" (GR1, OW1, FN1): data an
//! app manages and files that free nothing. Every row is listed and none can
//! be moved from here ([`super::Group::movable`]).
//!
//! - **Known app libraries** ([`LIBRARIES`]): UTM's virtual machines and Cap's
//!   recordings are bundles in a fixed folder. Each bundle is one item; what
//!   it frees is its private bytes (FG-L1). Reading is read-only: folder
//!   listings, file metadata, UTM's `config.plist` for the VM's name and the
//!   owning app's running state through the app probe.
//! - **Files that free nothing** (FN1): the scans hand over every large file
//!   whose blocks are all shared with a clone. If the clone's partner sits
//!   inside a known library (same APFS clone ID), the reason names it;
//!   otherwise it only says another copy uses the same space.

use std::collections::{HashMap, HashSet};
use std::fs;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::SystemTime;

use crate::clean::freed_size::{sharing_of, Sharing};
use crate::clean::fs_safe::PathFingerprint;
use crate::clean::private_size::{private_allocated_bytes, PrivateSize};
use crate::clean::process::AppProbe;

use super::installers::INSTALLER_EXTENSIONS;
use super::{Elsewhere, FreesNothing, Group, LargeFile};

/// How a library names its items and finds their pictures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Flavor {
    Utm,
    Cap,
}

/// One app whose library lives under the home folder.
#[derive(Debug, Clone, Copy)]
pub struct AppLibrary {
    pub id: &'static str,
    pub app_name: &'static str,
    pub bundle_id: &'static str,
    /// The process name `pgrep -x` matches.
    pub process_name: &'static str,
    /// Home-relative folder whose direct children are the items.
    pub folder: &'static str,
    /// Extension of an item's bundle folder, without the dot.
    pub item_extension: &'static str,
    /// What an item is called ("Virtual machine").
    pub kind: &'static str,
    flavor: Flavor,
}

/// The known app libraries (OW1). Both paths were read on a real Mac.
pub const LIBRARIES: &[AppLibrary] = &[
    AppLibrary {
        id: "utm",
        app_name: "UTM",
        bundle_id: "com.utmapp.UTM",
        process_name: "UTM",
        folder: "Library/Containers/com.utmapp.UTM/Data/Documents",
        item_extension: "utm",
        kind: "Virtual machine",
        flavor: Flavor::Utm,
    },
    AppLibrary {
        id: "cap",
        app_name: "Cap",
        bundle_id: "so.cap.desktop",
        process_name: "Cap",
        folder: "Library/Application Support/so.cap.desktop/recordings",
        item_extension: "cap",
        kind: "Screen recording",
        flavor: Flavor::Cap,
    },
];

/// What a group 5 scan reads, injectable so tests never touch the real home.
pub struct ElsewhereRoots<'a> {
    pub home: &'a Path,
    /// Folders whose direct `.app` children are installed apps.
    pub applications: &'a [PathBuf],
    pub probe: &'a dyn AppProbe,
}

/// One bundle of a library, with the words that name it.
struct Item {
    library: &'static AppLibrary,
    path: PathBuf,
    /// The row's name ("Windows · UTM virtual machine").
    name: String,
    /// How a sentence refers to it ("UTM's Windows VM").
    phrase: String,
}

/// Group 5's rows, largest first. `frees_nothing` is every large file the
/// other scans found with no private bytes; a path listed twice counts once.
/// A cancelled scan returns what it had.
pub fn scan_elsewhere(
    roots: &ElsewhereRoots<'_>,
    frees_nothing: Vec<FreesNothing>,
    cancel: Option<&AtomicBool>,
) -> Vec<LargeFile> {
    let cancelled = || cancel.is_some_and(|flag| flag.load(Ordering::Acquire));
    let items = library_items(roots.home);
    let mut running: HashMap<&str, bool> = HashMap::new();
    let mut app_of = |library: &AppLibrary| -> (Option<PathBuf>, bool) {
        let path = installed_app(roots.applications, library.app_name);
        let is_running = *running
            .entry(library.id)
            .or_insert_with(|| roots.probe.probe(library.process_name) == Ok(true));
        (path, is_running)
    };
    let mut rows = Vec::new();
    for item in &items {
        if cancelled() {
            return rows;
        }
        let Ok(meta) = fs::symlink_metadata(&item.path) else {
            continue;
        };
        let frees = private_allocated_bytes(&item.path).unwrap_or(PrivateSize {
            bytes: 0,
            exact: false,
        });
        let (app, app_running) = app_of(item.library);
        rows.push(LargeFile {
            path: item.path.clone(),
            name: item.name.clone(),
            group: Group::Elsewhere,
            reason: format!(
                "Remove it in {app}, not here · it sits in {app}'s library",
                app = item.library.app_name
            ),
            frees,
            matched_app: app,
            fingerprint: PathFingerprint::of(&meta),
            elsewhere: Some(Elsewhere {
                kind: item.library.kind.to_string(),
                app_name: Some(item.library.app_name.to_string()),
                app_bundle_id: Some(item.library.bundle_id.to_string()),
                app_running,
                preview: preview_of(item),
                shared_bytes: None,
                shared_with: None,
                last_changed: newest_change(&item.path),
            }),
        });
    }
    if !frees_nothing.is_empty() && !cancelled() {
        let partners = clone_partners(&items, cancel);
        let mut seen = HashSet::new();
        for file in frees_nothing {
            if cancelled() {
                break;
            }
            if seen.insert(file.path.clone()) {
                rows.push(frees_nothing_row(file, &items, &partners, &mut app_of));
            }
        }
    }
    rows.sort_by(|a, b| size_of(b).cmp(&size_of(a)).then(a.path.cmp(&b.path)));
    rows
}

/// What orders group 5: the size a row frees, or the size it shares.
fn size_of(file: &LargeFile) -> u64 {
    file.elsewhere
        .as_ref()
        .and_then(|e| e.shared_bytes)
        .unwrap_or(file.frees.bytes)
}

/// FN1: a file all of whose blocks are shared. The reason names the partner
/// only when its clone ID was found inside a known library.
fn frees_nothing_row(
    file: FreesNothing,
    items: &[Item],
    partners: &HashMap<u64, usize>,
    app_of: &mut impl FnMut(&AppLibrary) -> (Option<PathBuf>, bool),
) -> LargeFile {
    let sharing = sharing_of(&file.path);
    let partner = match sharing {
        Sharing::Clone { id, .. } => partners.get(&id).map(|index| &items[*index]),
        _ => None,
    };
    let reason = match (partner, sharing) {
        (Some(item), _) => format!("Frees nothing: {} holds the same data", item.phrase),
        (None, Sharing::Clone { .. }) => {
            "Frees nothing: another copy on this Mac uses the same space".to_string()
        }
        (None, _) => "Frees nothing: its space is shared with other files".to_string(),
    };
    let (app, app_running) = partner.map_or((None, false), |item| app_of(item.library));
    let extension = file
        .path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_lowercase);
    let kind = match extension {
        Some(e) if INSTALLER_EXTENSIONS.contains(&e.as_str()) => "Installer",
        _ => "File",
    };
    LargeFile {
        name: file
            .path
            .file_stem()
            .map_or_else(String::new, |s| s.to_string_lossy().into_owned()),
        path: file.path,
        group: Group::Elsewhere,
        reason,
        frees: PrivateSize {
            bytes: 0,
            exact: true,
        },
        matched_app: app,
        fingerprint: file.fingerprint,
        elsewhere: Some(Elsewhere {
            kind: kind.to_string(),
            app_name: partner.map(|item| item.library.app_name.to_string()),
            app_bundle_id: partner.map(|item| item.library.bundle_id.to_string()),
            app_running,
            preview: None,
            shared_bytes: Some(file.allocated),
            shared_with: partner.map(|item| item.phrase.clone()),
            last_changed: file.modified,
        }),
    }
}

/// The bundles of every library, library by library, in name order.
fn library_items(home: &Path) -> Vec<Item> {
    let mut items = Vec::new();
    for library in LIBRARIES {
        let Ok(entries) = fs::read_dir(home.join(library.folder)) else {
            continue;
        };
        let mut bundles: Vec<PathBuf> = entries
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case(library.item_extension))
                    && fs::symlink_metadata(path).is_ok_and(|m| m.is_dir())
            })
            .collect();
        bundles.sort();
        for path in bundles {
            let (name, phrase) = words_for(library, &path);
            items.push(Item {
                library,
                path,
                name,
                phrase,
            });
        }
    }
    items
}

fn words_for(library: &AppLibrary, bundle: &Path) -> (String, String) {
    let stem = bundle
        .file_stem()
        .map_or_else(String::new, |s| s.to_string_lossy().into_owned());
    match library.flavor {
        Flavor::Utm => {
            let vm = utm_name(bundle).unwrap_or(stem);
            (
                format!("{vm} · UTM virtual machine"),
                format!("UTM's {vm} VM"),
            )
        }
        Flavor::Cap => match cap_stamp(bundle, &stem) {
            Some(when) => (
                format!("Cap recording · {when}"),
                format!("the Cap recording from {when}"),
            ),
            None => (format!("Cap recording · {stem}"), format!("Cap's {stem}")),
        },
    }
}

/// UTM's `Information › Name`, read from the XML `config.plist`. A binary
/// plist or a missing key gives `None`, and the bundle's name stands in.
fn utm_name(bundle: &Path) -> Option<String> {
    let text = fs::read_to_string(bundle.join("config.plist")).ok()?;
    let info = &text[text.find("<key>Information</key>")?..];
    let info = &info[..info.find("</dict>")?];
    let after = &info[info.find("<key>Name</key>")? + "<key>Name</key>".len()..];
    let start = after.find("<string>")? + "<string>".len();
    let end = after[start..].find("</string>")? + start;
    let name = unescape(after[start..end].trim());
    (!name.is_empty()).then_some(name)
}

fn unescape(text: &str) -> String {
    text.replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// "30 Oct, 5:10 PM" from the recording's bundle name ("… 2026-10-30 05.10
/// PM"), else from `recording-meta.json`'s `pretty_name` ("Cap 2026-10-30 at
/// 17.40.53"). Both are local times, so no zone is applied.
fn cap_stamp(bundle: &Path, stem: &str) -> Option<String> {
    let from_meta = || {
        let text = fs::read_to_string(bundle.join("recording-meta.json")).ok()?;
        let meta: serde_json::Value = serde_json::from_str(&text).ok()?;
        meta.get("pretty_name")?.as_str().and_then(parse_stamp)
    };
    parse_stamp(stem).or_else(from_meta)
}

const MONTHS: [&str; 12] = [
    "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
];

/// Finds `YYYY-MM-DD`, then `HH.MM` (24 h, or 12 h with AM/PM after it).
fn parse_stamp(text: &str) -> Option<String> {
    let bytes = text.as_bytes();
    let number = |from: usize, digits: usize| -> Option<u32> {
        let slice = bytes.get(from..from + digits)?;
        slice
            .iter()
            .all(u8::is_ascii_digit)
            .then(|| slice.iter().fold(0, |n, d| n * 10 + u32::from(d - b'0')))
    };
    let at = (0..bytes.len()).find(|&i| {
        bytes.get(i + 4) == Some(&b'-')
            && bytes.get(i + 7) == Some(&b'-')
            && number(i, 4).is_some()
            && number(i + 5, 2).is_some()
            && number(i + 8, 2).is_some()
    })?;
    let (month, day) = (number(at + 5, 2)?, number(at + 8, 2)?);
    let mut time = at + 10;
    time += if text[time..].starts_with(" at ") {
        4
    } else {
        1
    };
    let (mut hour, minute) = (number(time, 2)?, number(time + 3, 2)?);
    if bytes.get(time + 2) != Some(&b'.') || !(1..=12).contains(&month) || hour > 23 || minute > 59
    {
        return None;
    }
    let rest = text[time + 5..].trim_start_matches(|c: char| c == '.' || c.is_ascii_digit());
    let rest = rest.trim_start();
    match rest.get(..2) {
        Some("PM") if hour < 12 => hour += 12,
        Some("AM") if hour == 12 => hour = 0,
        _ => {}
    }
    let (clock, suffix) = match hour {
        0 => (12, "AM"),
        1..=11 => (hour, "AM"),
        12 => (12, "PM"),
        _ => (hour - 12, "PM"),
    };
    Some(format!(
        "{day} {}, {clock}:{minute:02} {suffix}",
        MONTHS[month as usize - 1]
    ))
}

/// The picture the app keeps for an item, when it has one on disk.
fn preview_of(item: &Item) -> Option<PathBuf> {
    match item.library.flavor {
        Flavor::Utm => Some(item.path.join("screenshot.png")).filter(|p| p.is_file()),
        Flavor::Cap => {
            let folder = item.path.join("screenshots");
            let mut images: Vec<PathBuf> = fs::read_dir(folder)
                .ok()?
                .flatten()
                .map(|e| e.path())
                .filter(|p| {
                    p.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                        ["jpg", "jpeg", "png"].contains(&e.to_lowercase().as_str())
                    })
                })
                .collect();
            // `display.jpg` is the recording's own frame; a thumbnail is a fallback.
            images.sort_by_key(|p| {
                (
                    p.file_stem().is_none_or(|s| s != "display"),
                    p.file_name().map(ToOwned::to_owned),
                )
            });
            images.into_iter().next()
        }
    }
}

/// The newest modification date of any file inside the bundle.
fn newest_change(bundle: &Path) -> Option<SystemTime> {
    let mut newest: Option<SystemTime> = None;
    let mut pending = vec![bundle.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let Ok(meta) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if meta.is_dir() {
                pending.push(entry.path());
            } else if meta.is_file() {
                newest = newest.max(meta.modified().ok());
            }
        }
    }
    newest
}

/// APFS clone ID → the item holding a file with it. Walked only when a file
/// that frees nothing needs a partner, once per scan.
fn clone_partners(items: &[Item], cancel: Option<&AtomicBool>) -> HashMap<u64, usize> {
    let mut partners = HashMap::new();
    let mut seen: HashSet<(u64, u64)> = HashSet::new();
    for (index, item) in items.iter().enumerate() {
        let mut pending = vec![item.path.clone()];
        while let Some(dir) = pending.pop() {
            if cancel.is_some_and(|flag| flag.load(Ordering::Acquire)) {
                return partners;
            }
            let Ok(entries) = fs::read_dir(&dir) else {
                continue;
            };
            for entry in entries.flatten() {
                let path = entry.path();
                let Ok(meta) = fs::symlink_metadata(&path) else {
                    continue;
                };
                if meta.is_dir() {
                    pending.push(path);
                } else if meta.is_file() && seen.insert((meta.dev(), meta.ino())) {
                    if let Sharing::Clone { id, .. } = sharing_of(&path) {
                        partners.entry(id).or_insert(index);
                    }
                }
            }
        }
    }
    partners
}

/// The `.app` named `name` in the first folder that has one.
fn installed_app(folders: &[PathBuf], name: &str) -> Option<PathBuf> {
    folders
        .iter()
        .map(|folder| folder.join(format!("{name}.app")))
        .find(|path| fs::symlink_metadata(path).is_ok_and(|m| m.is_dir()))
}
