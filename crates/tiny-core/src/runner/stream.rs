//! `CommandRunner::stream`: a command read line by line while it runs, that
//! can be cancelled.
//!
//! `output` collects everything at exit, which is too late to show that
//! `brew` is downloading. Here both pipes feed one channel and the caller's
//! thread hands each line to the callback, so the callback needs no `Send`.
//! Cancellation and the deadline are polled between lines. A tool that must
//! stop is first interrupted (SIGINT, as Ctrl-C does, so `brew` can undo what
//! it began) and only killed after a grace period.

use std::io::{BufRead, BufReader, Read};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use super::{spawn_failure, CommandError, CommandOutcome, ToolRunner, POLL_INTERVAL};

/// Stdout kept in the outcome, from the start; callers read it line by line.
const STDOUT_KEPT: usize = 64 * 1024;
/// Stderr kept in the outcome, from the end: `brew` prints its `Error:` last.
const STDERR_KEPT: usize = 8 * 1024;
/// A line longer than this reaches the callback in pieces.
const LINE_LIMIT: u64 = 8 * 1024;
/// How long an interrupted tool has to finish before it is killed.
pub(super) const INTERRUPT_GRACE: Duration = Duration::from_secs(5);

/// How a streamed command ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEnd {
    Finished(CommandOutcome),
    /// The cancel flag was raised and the command was stopped.
    Cancelled,
}

pub(super) fn is_cancelled(cancel: Option<&AtomicBool>) -> bool {
    cancel.is_some_and(|flag| flag.load(Ordering::Acquire))
}

/// Which end of a pipe is worth keeping when it is longer than the limit.
#[derive(Clone, Copy)]
enum Keep {
    Start,
    End,
}

impl ToolRunner {
    pub(super) fn run_stream(
        &self,
        bin: &str,
        args: &[&str],
        timeout: Duration,
        cancel: Option<&AtomicBool>,
        grace: Duration,
        on_line: &mut dyn FnMut(&str),
    ) -> Result<StreamEnd, CommandError> {
        if is_cancelled(cancel) {
            return Ok(StreamEnd::Cancelled);
        }
        let command = self
            .lookup
            .command(bin)
            .ok_or_else(|| CommandError::NotFound(bin.to_string()))?;
        let mut child = self
            .spawn(command, args)
            .map_err(|e| spawn_failure(bin, e))?;
        let (lines, arrived) = mpsc::channel();
        let stdout = pump(child.stdout(), lines.clone(), STDOUT_KEPT, Keep::Start);
        let stderr = pump(child.stderr(), lines, STDERR_KEPT, Keep::End);
        let timed_out = || CommandError::Timeout {
            bin: bin.to_string(),
            timeout_ms: timeout.as_millis() as u64,
        };
        let deadline = Instant::now() + timeout;
        let hand_out = |bytes: Vec<u8>, on_line: &mut dyn FnMut(&str)| {
            let line = String::from_utf8_lossy(&bytes);
            on_line(line.trim_end_matches(['\n', '\r']));
        };

        let mut pipes_closed = false;
        let status = loop {
            // These checks run even while an inherited pipe keeps producing lines.
            if is_cancelled(cancel) {
                child
                    .interrupt(grace, || stdout.is_finished() && stderr.is_finished())
                    .map_err(|e| spawn_failure(bin, e))?;
                return Ok(StreamEnd::Cancelled);
            }
            if Instant::now() >= deadline {
                child
                    .interrupt(grace, || stdout.is_finished() && stderr.is_finished())
                    .map_err(|e| spawn_failure(bin, e))?;
                return Err(timed_out());
            }
            if pipes_closed {
                if child.exited().map_err(|e| spawn_failure(bin, e))? {
                    break child.wait().map_err(|e| spawn_failure(bin, e))?;
                }
                thread::sleep(POLL_INTERVAL);
                continue;
            }
            match arrived.recv_timeout(POLL_INTERVAL) {
                Ok(bytes) => hand_out(bytes, on_line),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => pipes_closed = true,
            }
        };
        let kept = |handle: JoinHandle<Vec<u8>>| {
            String::from_utf8_lossy(&handle.join().unwrap_or_default()).into_owned()
        };
        Ok(StreamEnd::Finished(CommandOutcome {
            status: status.code(),
            stdout: kept(stdout),
            stderr: kept(stderr),
        }))
    }
}

/// Sends each line of `pipe` to `lines` and returns up to `limit` bytes of it.
/// After the receiver is gone it keeps reading, so the child never blocks on
/// a full pipe.
fn pump<R: Read + Send + 'static>(
    pipe: Option<R>,
    lines: Sender<Vec<u8>>,
    limit: usize,
    keep: Keep,
) -> JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut kept = Vec::new();
        let Some(pipe) = pipe else { return kept };
        let mut reader = BufReader::new(pipe);
        let mut line = Vec::new();
        let mut listening = true;
        loop {
            line.clear();
            match (&mut reader).take(LINE_LIMIT).read_until(b'\n', &mut line) {
                Ok(0) | Err(_) => break,
                Ok(_) => {}
            }
            match keep {
                Keep::Start => {
                    let room = limit.saturating_sub(kept.len());
                    kept.extend_from_slice(&line[..line.len().min(room)]);
                }
                Keep::End => {
                    kept.extend_from_slice(&line);
                    if kept.len() > 2 * limit {
                        kept.drain(..kept.len() - limit);
                    }
                }
            }
            if listening && lines.send(line.clone()).is_err() {
                listening = false;
            }
        }
        if let Keep::End = keep {
            kept.drain(..kept.len().saturating_sub(limit));
        }
        kept
    })
}

#[cfg(test)]
mod tests;
