use crate::error::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Serialize, Deserialize, Debug)]
pub struct FocusSession {
    pub started_at_unix: u64,
    pub finished_at_unix: u64,
    pub minutes: u64,
    pub label: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Default)]
struct FocusLog {
    sessions: Vec<FocusSession>,
}

pub fn to_unix(time: SystemTime) -> u64 {
    time.duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

pub fn log_path() -> Result<PathBuf> {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .context("could not resolve user home directory")?;
    let dir = home.join(".tiny-cli");
    fs::create_dir_all(&dir).with_context(|| format!("failed to create {}", dir.display()))?;
    Ok(dir.join("focus-sessions.json"))
}

pub fn append_session(path: &PathBuf, session: FocusSession) -> Result<()> {
    let mut log: FocusLog = if path.exists() {
        let raw = fs::read_to_string(path)
            .with_context(|| format!("failed to read {}", path.display()))?;
        if raw.trim().is_empty() {
            FocusLog::default()
        } else {
            match serde_json::from_str::<FocusLog>(&raw) {
                Ok(log) => log,
                Err(e) => {
                    let backup = path.with_extension("json.bak");
                    fs::copy(path, &backup)
                        .with_context(|| format!("failed to back up {}", path.display()))?;
                    return Err(crate::engine_error!(
                        "{} is not valid focus log JSON ({}). Backed up to {} — fix or delete it before retrying.",
                        path.display(),
                        e,
                        backup.display()
                    ));
                }
            }
        }
    } else {
        FocusLog::default()
    };

    log.sessions.push(session);
    let serialized = serde_json::to_string_pretty(&log)?;
    fs::write(path, serialized).with_context(|| format!("failed to write {}", path.display()))?;
    Ok(())
}
