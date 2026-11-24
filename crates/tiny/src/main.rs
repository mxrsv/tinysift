mod cli;
mod render;
mod tui;
mod util;

use anyhow::Result;
use clap::Parser;

use crate::cli::{Cli, Commands};

fn main() -> Result<()> {
    // Bare `sift` in a terminal opens the TUI; anything else parses as before.
    match tui::launch()? {
        tui::Launch::NotRequested => {}
        tui::Launch::Done(0) => return Ok(()),
        tui::Launch::Done(status) => std::process::exit(status),
    }
    let cli = Cli::parse();

    match cli.command {
        Commands::Sys => render::sys::run(),
        Commands::Scan(opts) => render::scan::run(opts),
        Commands::Focus(opts) => render::focus::run(opts),
        Commands::Uninstall(opts) => render::uninstall::run(opts),
        Commands::Clean(opts) => render::clean::run(opts),
        Commands::Processes(opts) => render::processes::run(opts),
    }
}
