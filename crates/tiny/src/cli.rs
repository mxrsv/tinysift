use std::path::PathBuf;

use clap::{Args, Parser, Subcommand, ValueEnum};

#[derive(Parser, Debug)]
#[command(
    name = "tiny",
    version,
    about = "A small, practical CLI for performance and productivity",
    long_about = None
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Show system information (CPU, memory, disk, uptime)
    Sys,

    /// Scan common folders and report large or old files (read-only)
    Scan(ScanOpts),

    /// Run a focus timer session and log it locally
    Focus(FocusOpts),

    /// Uninstall apps from /Applications and clean ~/Library leftovers
    Uninstall(UninstallOpts),

    /// Interactive cleanup of developer caches and recoverable data
    Clean(CleanOpts),

    /// List, inspect and quit your processes
    Processes(ProcessesOpts),
}

#[derive(Args, Debug)]
#[command(args_conflicts_with_subcommands = true)]
pub struct ProcessesOpts {
    #[command(subcommand)]
    pub action: Option<ProcessesAction>,

    /// Sort order applied to the list.
    #[arg(long, value_enum, default_value_t = ProcessSort::Cpu)]
    pub sort: ProcessSort,

    /// Maximum number of processes to list. Ignored with --port, which lists
    /// every visible owner.
    #[arg(long, default_value_t = 20)]
    pub limit: usize,

    /// Emit a machine-readable JSON report on stdout instead of text.
    #[arg(long)]
    pub json: bool,

    /// Only list visible owners of this listening TCP port.
    #[arg(long, value_name = "PORT")]
    pub port: Option<u16>,
}

#[derive(Subcommand, Debug)]
pub enum ProcessesAction {
    /// Show one process with its parent, children and listening ports
    Show {
        pid: u32,

        /// Emit JSON instead of text.
        #[arg(long)]
        json: bool,
    },

    /// Ask a process to quit (SIGTERM), or end it at once with --force (SIGKILL)
    Quit {
        pid: u32,

        /// Send SIGKILL instead of SIGTERM. Unsaved work is lost.
        #[arg(long)]
        force: bool,

        /// Skip the confirmation prompt. With --force this also requires
        /// TINY_CONFIRM_FORCE=1.
        #[arg(short = 'y', long)]
        yes: bool,
    },
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessSort {
    /// Highest CPU first
    Cpu,
    /// Highest resident memory first
    Memory,
}

#[derive(Args, Debug)]
pub struct ScanOpts {
    /// Report files at or above this size in megabytes
    #[arg(long, default_value_t = 100)]
    pub min_size_mb: u64,

    /// Report files older than this many days
    #[arg(long, default_value_t = 90)]
    pub older_than_days: u64,

    /// Custom directory to scan. Repeatable. Overrides the default
    /// Downloads/Desktop/Documents set when provided at least once.
    #[arg(long, value_name = "DIR")]
    pub path: Vec<PathBuf>,

    /// Emit a machine-readable JSON report on stdout instead of text.
    #[arg(long)]
    pub json: bool,

    /// Maximum number of items to display per section (text mode).
    #[arg(long, default_value_t = 20)]
    pub limit: usize,

    /// Sort order applied to file listings.
    #[arg(long, value_enum, default_value_t = ScanSort::Size)]
    pub sort: ScanSort,

    /// Group duplicate files by (size + filename).
    #[arg(long)]
    pub duplicates: bool,

    /// Verify duplicate groups by hashing file contents. Slower but accurate.
    #[arg(long, requires = "duplicates")]
    pub hash: bool,

    /// Print a per-extension breakdown (count + total size).
    #[arg(long)]
    pub by_ext: bool,
}

#[derive(ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum ScanSort {
    /// Largest files first
    Size,
    /// Oldest files first
    Age,
    /// Lexicographic path order
    Path,
}

#[derive(Args, Debug)]
pub struct UninstallOpts {
    /// App name (e.g. "Cursor"). If omitted, an interactive picker is shown.
    pub name: Option<String>,

    /// Only show the report and exit. Does not prompt for action.
    #[arg(long)]
    pub dry_run: bool,

    /// Skip the action menu and execute immediately (Trash, or rm -rf if --hard).
    #[arg(short = 'y', long)]
    pub yes: bool,

    /// Only remove /Applications/<Name>.app, skip ~/Library cleanup.
    #[arg(long, conflicts_with = "leftovers_only")]
    pub shallow: bool,

    /// Only clean ~/Library leftovers, keep /Applications/<Name>.app.
    #[arg(long, conflicts_with = "shallow")]
    pub leftovers_only: bool,

    /// rm -rf instead of moving to Trash. NOT recoverable.
    #[arg(long)]
    pub hard: bool,

    /// Sort order in the interactive picker.
    #[arg(long, value_enum, default_value_t = SortBy::LastUsed)]
    pub sort: SortBy,

    /// Allow uninstalling Homebrew casks (default: warn and refuse).
    #[arg(long)]
    pub force: bool,
}

#[derive(ValueEnum, Clone, Debug, PartialEq, Eq)]
pub enum SortBy {
    /// Least recently used first (default)
    LastUsed,
    /// Largest size first
    Size,
    /// Alphabetical
    Name,
}

#[derive(Args, Debug)]
pub struct CleanOpts {
    /// Show the report and exit without prompting for cleanup.
    #[arg(long, conflicts_with = "yes")]
    pub dry_run: bool,

    /// Skip the picker and the action menu. Requires --category.
    #[arg(short = 'y', long)]
    pub yes: bool,

    /// Use permanent deletion instead of Move to Trash. NOT recoverable.
    #[arg(long)]
    pub hard: bool,

    /// Restrict discovery and selection to the named category. Repeatable.
    #[arg(long, action = clap::ArgAction::Append)]
    pub category: Vec<String>,

    /// Show review-risk categories in the picker (unchecked).
    #[arg(long)]
    pub include_review: bool,

    /// Show destructive categories such as Trash in the picker (unchecked).
    #[arg(long)]
    pub include_destructive: bool,

    /// Open a per-path drill-down stage after picking categories. Lets you
    /// uncheck specific paths before execute. Conflicts with --yes.
    #[arg(long, conflicts_with = "yes")]
    pub review_paths: bool,

    /// Idle threshold (in days) for providers that filter by mtime
    /// (node_modules, rust target/, downloads_old, ...). Must be > 0.
    #[arg(long, default_value_t = 30)]
    pub idle_days: u64,
}

#[derive(Args, Debug)]
pub struct FocusOpts {
    /// Length of the focus session in minutes
    #[arg(long, default_value_t = 25)]
    pub minutes: u64,

    /// Optional label for the session (e.g. "deep work")
    #[arg(long)]
    pub label: Option<String>,
}

impl From<&CleanOpts> for tiny_core::options::CleanOptions {
    fn from(value: &CleanOpts) -> Self {
        Self {
            category: value.category.clone(),
            include_review: value.include_review,
            include_destructive: value.include_destructive,
            idle_days: value.idle_days,
        }
    }
}
impl From<&ScanOpts> for tiny_core::options::ScanOptions {
    fn from(value: &ScanOpts) -> Self {
        Self {
            min_size_mb: value.min_size_mb,
            older_than_days: value.older_than_days,
            path: value.path.clone(),
        }
    }
}
impl From<&UninstallOpts> for tiny_core::options::UninstallOptions {
    fn from(value: &UninstallOpts) -> Self {
        Self {
            shallow: value.shallow,
            leftovers_only: value.leftovers_only,
            force: value.force,
        }
    }
}
impl From<ScanSort> for tiny_core::options::ScanSort {
    fn from(value: ScanSort) -> Self {
        match value {
            ScanSort::Size => Self::Size,
            ScanSort::Age => Self::Age,
            ScanSort::Path => Self::Path,
        }
    }
}
impl From<ProcessSort> for tiny_core::processes::ProcessSort {
    fn from(value: ProcessSort) -> Self {
        match value {
            ProcessSort::Cpu => Self::Cpu,
            ProcessSort::Memory => Self::Memory,
        }
    }
}
impl From<SortBy> for tiny_core::options::SortBy {
    fn from(value: SortBy) -> Self {
        match value {
            SortBy::LastUsed => Self::LastUsed,
            SortBy::Size => Self::Size,
            SortBy::Name => Self::Name,
        }
    }
}
