use std::io::{self, Write};
use std::thread;
use std::time::{Duration, SystemTime};

use anyhow::Result;
use tiny_core::focus::{append_session, log_path, to_unix, FocusSession};

use crate::cli::FocusOpts;

pub fn run(opts: FocusOpts) -> Result<()> {
    if opts.minutes == 0 {
        println!("minutes must be > 0");
        return Ok(());
    }

    let started_at = SystemTime::now();
    let label = opts.label.as_deref().unwrap_or("focus");
    println!("Starting {} session: {} minute(s)", label, opts.minutes);
    println!("Press Ctrl+C to abort.");

    let total_secs = opts
        .minutes
        .checked_mul(60)
        .ok_or_else(|| anyhow::anyhow!("minutes is too large"))?;
    run_timer(total_secs);

    let finished_at = SystemTime::now();
    let session = FocusSession {
        started_at_unix: to_unix(started_at),
        finished_at_unix: to_unix(finished_at),
        minutes: opts.minutes,
        label: opts.label,
    };

    let log_path = log_path()?;
    append_session(&log_path, session)?;
    println!();
    println!("Session complete. Logged to {}", log_path.display());
    Ok(())
}

fn run_timer(total_secs: u64) {
    let bar_width: u64 = 30;
    for elapsed in 0..=total_secs {
        let remaining = total_secs - elapsed;
        let filled = elapsed
            .saturating_mul(bar_width)
            .checked_div(total_secs)
            .unwrap_or(bar_width);
        let empty = bar_width - filled;
        let bar = format!(
            "{}{}",
            "#".repeat(filled as usize),
            "-".repeat(empty as usize)
        );
        let mins = remaining / 60;
        let secs = remaining % 60;
        print!("\r[{}] {:02}:{:02} remaining", bar, mins, secs);
        let _ = io::stdout().flush();

        if elapsed < total_secs {
            thread::sleep(Duration::from_secs(1));
        }
    }
}
