//! Human-readable reporter for the scan command.

use crate::cli::ScanOpts;
use crate::util::format_bytes;

use super::duplicates::DuplicateGroup;
use super::extensions::ExtStat;
use super::sort::sort_files;
use super::types::{FileEntry, ScanData};

pub fn render(data: &ScanData, opts: &ScanOpts) {
    print_header(data, opts);

    let mut large = data.large.clone();
    let mut old = data.old.clone();
    sort_files(&mut large, opts.sort.into());
    sort_files(&mut old, opts.sort.into());

    print_files("Large files", &large, opts.limit, FileFormat::Size);
    println!();
    print_files("Old files", &old, opts.limit, FileFormat::Age);

    if opts.by_ext {
        println!();
        let stats = super::extensions::aggregate(&data.all_files);
        print_extensions(&stats, opts.limit);
    }

    if opts.duplicates {
        println!();
        let groups = super::duplicates::find(&data.all_files, opts.hash);
        print_duplicates(&groups, opts.limit);
    }
}

fn print_header(data: &ScanData, opts: &ScanOpts) {
    let roots: Vec<String> = data.roots.iter().map(|r| r.display().to_string()).collect();
    println!("Scan roots:");
    for r in &roots {
        println!("  {}", r);
    }
    println!(
        "Thresholds: size >= {} MB, age >= {} days",
        opts.min_size_mb, opts.older_than_days
    );
    println!("Sort: {:?}, Limit: {}", opts.sort, opts.limit);
    if opts.duplicates {
        println!(
            "Duplicate detection: enabled (verify_with_hash = {})",
            opts.hash
        );
    }
    println!("(read-only — no files are deleted)");
    println!();
}

enum FileFormat {
    Size,
    Age,
}

fn print_files(title: &str, files: &[FileEntry], limit: usize, fmt: FileFormat) {
    println!("== {} ({}) ==", title, files.len());
    for entry in files.iter().take(limit) {
        match fmt {
            FileFormat::Size => {
                println!("{:>10}  {}", format_bytes(entry.size), entry.path.display())
            }
            FileFormat::Age => {
                println!("{:>5}d  {}", entry.age_secs / 86_400, entry.path.display())
            }
        }
    }
    if files.len() > limit {
        println!("... and {} more", files.len() - limit);
    }
}

fn print_extensions(stats: &[ExtStat], limit: usize) {
    println!("== By extension ({}) ==", stats.len());
    println!("{:<14} {:>8} {:>14}", "EXT", "COUNT", "TOTAL");
    for s in stats.iter().take(limit) {
        println!(
            "{:<14} {:>8} {:>14}",
            s.ext,
            s.count,
            format_bytes(s.total_size)
        );
    }
    if stats.len() > limit {
        println!("... and {} more", stats.len() - limit);
    }
}

fn print_duplicates(groups: &[DuplicateGroup], limit: usize) {
    let total_wasted: u64 = groups.iter().map(|g| g.wasted_bytes()).sum();
    println!(
        "== Duplicates ({}, wasted ~{}) ==",
        groups.len(),
        format_bytes(total_wasted)
    );
    for g in groups.iter().take(limit) {
        let suffix = if g.verified_by_hash {
            " [hash-verified]"
        } else {
            ""
        };
        println!(
            "{:>10} x{}  {}{}",
            format_bytes(g.size),
            g.paths.len(),
            g.name,
            suffix
        );
        for p in &g.paths {
            println!("           - {}", p.display());
        }
    }
    if groups.len() > limit {
        println!("... and {} more", groups.len() - limit);
    }
}
