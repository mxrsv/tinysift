mod cli;
mod render;
mod util;

use anyhow::Result;
use clap::Parser;

use crate::cli::{Cli, Commands};

fn main() -> Result<()> {
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
