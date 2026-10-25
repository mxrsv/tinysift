use crate::error::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs::{self, File};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

const TEMP_ATTEMPTS: usize = 16;
static NEXT_TEMP: AtomicU64 = AtomicU64::new(0);

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
    // Lock a stable sibling: locking the JSON inode would stop protecting it
    // as soon as another writer atomically replaces that inode.
    let lock_path = path.with_extension("json.lock");
    let lock = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&lock_path)
        .with_context(|| format!("failed to open {}", lock_path.display()))?;
    lock.lock()
        .with_context(|| format!("failed to lock {}", lock_path.display()))?;

    let log: FocusLog = match fs::read_to_string(path) {
        Ok(raw) => {
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
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => FocusLog::default(),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };

    let updated = FocusLog {
        sessions: log
            .sessions
            .into_iter()
            .chain(std::iter::once(session))
            .collect(),
    };
    let serialized = serde_json::to_vec_pretty(&updated)?;
    replace_log(path, &serialized, |file, bytes| {
        file.write_all(bytes)?;
        file.sync_all()
    })
    .with_context(|| format!("failed to save {}", path.display()))?;
    Ok(())
}

fn replace_log(
    path: &Path,
    bytes: &[u8],
    write: impl FnOnce(&mut File, &[u8]) -> io::Result<()>,
) -> io::Result<()> {
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let (temporary, mut file) = temporary_log(parent)?;
    let saved = write(&mut file, bytes).and_then(|()| fs::rename(&temporary, path));
    if saved.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    saved?;
    // The JSON is already published if directory sync fails; keep its data
    // and report that durability error rather than retrying the append.
    File::open(parent)?.sync_all()
}

fn temporary_log(parent: &Path) -> io::Result<(PathBuf, File)> {
    for _ in 0..TEMP_ATTEMPTS {
        let sequence = NEXT_TEMP.fetch_add(1, Ordering::Relaxed);
        let path = parent.join(format!(".focus-{}-{sequence}.tmp", std::process::id()));
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
        "cannot reserve a focus log temporary file",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};

    #[test]
    fn concurrent_appends_keep_every_completed_session() {
        const WRITERS: usize = 16;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sessions.json");
        let seed = FocusLog {
            sessions: vec![FocusSession {
                started_at_unix: 0,
                finished_at_unix: 1,
                minutes: 1,
                label: Some("initial history".repeat(4096)),
            }],
        };
        fs::write(&path, serde_json::to_vec(&seed).unwrap()).unwrap();
        let ready = Arc::new(Barrier::new(WRITERS));
        let workers: Vec<_> = (1..=WRITERS)
            .map(|id| {
                let path = path.clone();
                let ready = ready.clone();
                std::thread::spawn(move || {
                    ready.wait();
                    append_session(
                        &path,
                        FocusSession {
                            started_at_unix: id as u64,
                            finished_at_unix: id as u64 + 1,
                            minutes: 1,
                            label: None,
                        },
                    )
                })
            })
            .collect();
        for worker in workers {
            worker.join().unwrap().unwrap();
        }
        let log: FocusLog = serde_json::from_slice(&fs::read(path).unwrap()).unwrap();
        let mut ids: Vec<_> = log.sessions.iter().map(|s| s.started_at_unix).collect();
        ids.sort_unstable();
        assert_eq!(ids, (0..=WRITERS as u64).collect::<Vec<_>>());
        let original = log.sessions.iter().find(|s| s.started_at_unix == 0).unwrap();
        assert_eq!(original.finished_at_unix, seed.sessions[0].finished_at_unix);
        assert_eq!(original.minutes, seed.sessions[0].minutes);
        assert_eq!(original.label, seed.sessions[0].label);
        for session in log.sessions.iter().filter(|s| s.started_at_unix != 0) {
            assert_eq!(session.finished_at_unix, session.started_at_unix + 1);
            assert_eq!(session.minutes, 1);
            assert_eq!(session.label, None);
        }
    }

    #[test]
    fn partial_write_failure_keeps_previous_history() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sessions.json");
        let previous = br#"{"sessions":[]}"#;
        fs::write(&path, previous).unwrap();
        let error = replace_log(&path, b"replacement", |file, _| {
            file.write_all(b"partial")?;
            Err(io::Error::other("injected disk write failure"))
        })
        .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other);
        assert_eq!(fs::read(&path).unwrap(), previous);
        assert_eq!(fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn malformed_history_is_preserved_instead_of_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("sessions.json");
        fs::write(&path, b"not JSON").unwrap();
        let result = append_session(
            &path,
            FocusSession {
                started_at_unix: 1,
                finished_at_unix: 2,
                minutes: 1,
                label: None,
            },
        );
        assert!(result.is_err());
        assert_eq!(fs::read(path).unwrap(), b"not JSON");
    }
}
