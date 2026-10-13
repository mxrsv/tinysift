use crate::util::{format_bytes, format_duration};
use anyhow::Result;

pub fn run() -> Result<()> {
    let info = tiny_core::sys::information();
    println!("== System ==");
    if let Some(os) = info.os {
        println!("OS:        {}", os);
    }
    if let Some(host) = info.host {
        println!("Host:      {}", host);
    }
    println!(
        "Uptime:    {}",
        info.uptime_seconds
            .map(format_duration)
            .unwrap_or_else(|| "unavailable".into())
    );
    println!(
        "\n== CPU ==\nCores:     {}\nModel:     {}",
        info.cpu_count, info.cpu_model
    );
    println!(
        "\n== Memory ==\nUsed:      {}\nTotal:     {}",
        format_bytes(info.memory_used),
        format_bytes(info.memory_total)
    );
    if info.memory_total > 0 {
        println!(
            "Usage:     {:.1}%",
            info.memory_used as f64 / info.memory_total as f64 * 100.0
        );
    }
    println!("\n== Disks ==");
    if info.disks.is_empty() {
        println!("(no disks reported)");
    }
    for disk in info.disks {
        println!(
            "{:<20} {} used / {} total ({})",
            disk.mount_point,
            format_bytes(disk.total_bytes.saturating_sub(disk.available_bytes)),
            format_bytes(disk.total_bytes),
            disk.name
        );
    }
    Ok(())
}
