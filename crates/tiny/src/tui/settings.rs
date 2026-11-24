//! The remembered `--icons` choice, in `~/.tiny-cli/tui-settings.json`.
//!
//! Writes follow the focus log's habits (`tiny_core::focus`): an advisory lock
//! on a stable sibling, a synced temporary file renamed over the target, and
//! owner-only modes. A file that cannot be read or parsed is never replaced:
//! the session draws Unicode and says why once, after the TUI closes.

use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use anyhow::{anyhow, Context, Result};
use serde_json::{Map, Value};

use super::icons::IconMode;

const DIR: &str = ".tiny-cli";
const FILE: &str = "tui-settings.json";
const ICONS_KEY: &str = "icons";
const TEMP_ATTEMPTS: usize = 16;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

/// What the settings file holds.
#[derive(Debug, PartialEq, Eq)]
pub enum Stored {
    Missing,
    Valid(Option<IconMode>),
    Invalid(String),
}

/// The icon mode for this session and any notice to print after it.
#[derive(Debug, PartialEq, Eq)]
pub struct IconSetting {
    pub mode: IconMode,
    pub notice: Option<String>,
}

/// `$HOME/.tiny-cli/tui-settings.json`, or `None` without a home.
pub fn settings_path() -> Option<PathBuf> {
    std::env::var_os("HOME").map(|home| PathBuf::from(home).join(DIR).join(FILE))
}

/// Applies `--icons` (stored for later runs) or reads the remembered choice.
pub fn resolve(path: Option<&Path>, requested: Option<IconMode>) -> IconSetting {
    let Some(path) = path else {
        return IconSetting {
            mode: requested.unwrap_or_default(),
            notice: requested.map(|_| "--icons was not saved: HOME is not set".to_string()),
        };
    };
    if let Some(mode) = requested {
        let notice = save_icons(path, mode)
            .err()
            .map(|error| format!("--icons {} was not saved: {error:#}", mode.name()));
        return IconSetting { mode, notice };
    }
    match read(path) {
        Stored::Missing => IconSetting {
            mode: IconMode::Unicode,
            notice: None,
        },
        Stored::Valid(mode) => IconSetting {
            mode: mode.unwrap_or_default(),
            notice: None,
        },
        Stored::Invalid(reason) => IconSetting {
            mode: IconMode::Unicode,
            notice: Some(format!(
                "{} {reason}, so Unicode icons were drawn; fix or delete it",
                path.display()
            )),
        },
    }
}

pub fn read(path: &Path) -> Stored {
    match fs::read_to_string(path) {
        Ok(raw) => parse(&raw).map_or_else(Stored::Invalid, |doc| Stored::Valid(doc.1)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Stored::Missing,
        Err(error) => Stored::Invalid(format!("could not be read ({error})")),
    }
}

/// The JSON object and its icon mode; unknown keys are kept for later versions.
fn parse(raw: &str) -> std::result::Result<(Map<String, Value>, Option<IconMode>), String> {
    let doc = match serde_json::from_str::<Value>(raw) {
        Ok(Value::Object(doc)) => doc,
        Ok(_) => return Err("is not a JSON object".into()),
        Err(error) => return Err(format!("is not valid JSON ({error})")),
    };
    let mode = match doc.get(ICONS_KEY) {
        None => None,
        Some(Value::String(name)) => Some(
            IconMode::from_name(name)
                .ok_or_else(|| format!("names an unknown icon set \"{name}\""))?,
        ),
        Some(_) => return Err("has an \"icons\" value that is not a string".into()),
    };
    Ok((doc, mode))
}

/// Stores `mode` under the lock; an existing file that is not valid stays as it is.
pub fn save_icons(path: &Path, mode: IconMode) -> Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    fs::create_dir_all(parent).with_context(|| format!("cannot create {}", parent.display()))?;
    let lock_path = path.with_extension("json.lock");
    let lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&lock_path)
        .with_context(|| format!("cannot open {}", lock_path.display()))?;
    lock.lock()
        .with_context(|| format!("cannot lock {}", lock_path.display()))?;

    let mut doc = match fs::read_to_string(path) {
        Ok(raw) => {
            parse(&raw)
                .map_err(|reason| anyhow!("{} {reason}; it was left as it is", path.display()))?
                .0
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Map::new(),
        Err(error) => {
            return Err(error).with_context(|| format!("cannot read {}", path.display()));
        }
    };
    doc.insert(ICONS_KEY.into(), Value::String(mode.name().into()));
    let mut bytes = serde_json::to_vec_pretty(&Value::Object(doc))?;
    bytes.push(b'\n');
    replace(parent, path, &bytes).with_context(|| format!("cannot save {}", path.display()))
}

fn replace(parent: &Path, path: &Path, bytes: &[u8]) -> io::Result<()> {
    let (temporary, mut file) = temporary(parent)?;
    let saved = file
        .write_all(bytes)
        .and_then(|()| file.sync_all())
        .and_then(|()| fs::rename(&temporary, path));
    if saved.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    saved?;
    File::open(parent)?.sync_all()
}

fn temporary(parent: &Path) -> io::Result<(PathBuf, File)> {
    for _ in 0..TEMP_ATTEMPTS {
        let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(
            ".tui-settings-{}-{sequence}.tmp",
            std::process::id()
        ));
        match File::options()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => return Ok((path, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "cannot reserve a settings temporary file",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_path() -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(DIR).join(FILE);
        (dir, path)
    }

    #[test]
    fn a_missing_file_draws_unicode_silently() {
        let (_dir, path) = temp_path();
        let setting = resolve(Some(&path), None);
        assert_eq!(setting.mode, IconMode::Unicode);
        assert_eq!(setting.notice, None);
        assert!(!path.exists(), "reading must not create the file");
    }

    #[test]
    fn the_choice_is_remembered_for_later_runs() {
        let (_dir, path) = temp_path();
        assert_eq!(resolve(Some(&path), Some(IconMode::Nerd)).notice, None);
        assert_eq!(resolve(Some(&path), None).mode, IconMode::Nerd);
        assert_eq!(resolve(Some(&path), Some(IconMode::Unicode)).notice, None);
        assert_eq!(resolve(Some(&path), None).mode, IconMode::Unicode);
    }

    #[test]
    fn saving_keeps_keys_it_does_not_know() {
        let (_dir, path) = temp_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, r#"{"later": 1, "icons": "unicode"}"#).unwrap();
        save_icons(&path, IconMode::Nerd).unwrap();
        let doc: Value = serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(doc["later"], 1);
        assert_eq!(doc["icons"], "nerd");
    }

    #[test]
    fn an_invalid_file_falls_back_to_unicode_and_is_never_overwritten() {
        let (_dir, path) = temp_path();
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        for raw in [
            "not JSON",
            "[1]",
            r#"{"icons": "emoji"}"#,
            r#"{"icons": 3}"#,
        ] {
            fs::write(&path, raw).unwrap();
            let read_back = resolve(Some(&path), None);
            assert_eq!(read_back.mode, IconMode::Unicode, "{raw}");
            assert!(read_back.notice.is_some(), "{raw}");

            let saved = resolve(Some(&path), Some(IconMode::Nerd));
            assert_eq!(saved.mode, IconMode::Nerd, "this run still uses the flag");
            assert!(saved.notice.unwrap().contains("left as it is"), "{raw}");
            assert_eq!(fs::read_to_string(&path).unwrap(), raw);
        }
    }

    #[test]
    fn an_unreadable_file_falls_back_with_a_notice() {
        let (_dir, path) = temp_path();
        fs::create_dir_all(&path).unwrap(); // a directory cannot be read as a file
        let setting = resolve(Some(&path), None);
        assert_eq!(setting.mode, IconMode::Unicode);
        assert!(setting.notice.unwrap().contains("could not be read"));
    }

    #[test]
    fn saved_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let (_dir, path) = temp_path();
        save_icons(&path, IconMode::Nerd).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}
