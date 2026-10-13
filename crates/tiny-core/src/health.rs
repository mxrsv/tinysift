use crate::{clean::discover::DiscoveryReport, sys::SystemInfo};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthFactor {
    pub label: String,
    pub penalty: u8,
    pub explanation: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HealthScore {
    pub score: u8,
    pub reclaimable_bytes: u64,
    pub factors: Vec<HealthFactor>,
}

/// A transparent storage/pressure indicator, not a hardware diagnosis or an AI claim.
pub fn score(system: &SystemInfo, discovery: &DiscoveryReport) -> HealthScore {
    let disk = system
        .disks
        .iter()
        .filter(|d| d.total_bytes > 0)
        .find(|d| d.mount_point == "/")
        .or_else(|| system.disks.iter().find(|d| d.total_bytes > 0));
    let free_percent = disk.map(|d| d.available_bytes as f64 / d.total_bytes as f64 * 100.0);
    let disk_penalty = match free_percent {
        Some(p) if p < 5.0 => 45,
        Some(p) if p < 10.0 => 30,
        Some(p) if p < 20.0 => 15,
        _ => 0,
    };
    let memory_percent = if system.memory_total > 0 {
        system.memory_used as f64 / system.memory_total as f64 * 100.0
    } else {
        0.0
    };
    let memory_penalty = if memory_percent > 90.0 {
        15
    } else if memory_percent > 80.0 {
        8
    } else {
        0
    };
    let reclaimable_bytes = discovery
        .groups
        .iter()
        .filter(|g| is_recoverable(&g.id))
        .map(|g| g.total_size)
        .fold(0u64, u64::saturating_add);
    let cleanup_penalty = ((reclaimable_bytes / (1024 * 1024 * 1024)) * 2).min(20) as u8;
    HealthScore { score: 100 - disk_penalty - memory_penalty - cleanup_penalty, reclaimable_bytes,
        factors: vec![
            HealthFactor { label: "Free disk space".into(), penalty: disk_penalty, explanation: free_percent.map(|p| format!("{p:.1}% free; penalties apply below 20%, 10%, and 5%.")).unwrap_or_else(|| "Disk capacity unavailable; no penalty applied.".into()) },
            HealthFactor { label: "Memory use".into(), penalty: memory_penalty, explanation: format!("{memory_percent:.1}% used; pressure indicator only, includes OS caches.") },
            HealthFactor { label: "Cleanup opportunities".into(), penalty: cleanup_penalty, explanation: "2 points per GiB of recoverable candidates, capped at 20. Categories ranked by safety, then size.".into() },
        ] }
}
/// Providers that call irreversible external tools remain report-only in the GUI.
pub fn is_recoverable(id: &str) -> bool {
    !matches!(id, "trash" | "time-machine-local" | "docker")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sys::DiskInfo;
    #[test]
    fn score_explains_pressure_and_stays_in_range() {
        let sys = SystemInfo {
            os: None,
            host: None,
            uptime_seconds: None,
            cpu_count: 1,
            cpu_model: String::new(),
            cpu_usage: 0.0,
            memory_total: 100,
            memory_used: 95,
            disks: vec![DiskInfo {
                name: "disk".into(),
                mount_point: "/".into(),
                total_bytes: 100,
                available_bytes: 4,
            }],
        };
        let discovery = DiscoveryReport {
            groups: vec![],
            skipped_running: vec![],
        };
        let result = score(&sys, &discovery);
        assert_eq!(result.score, 40);
        assert_eq!(result.factors[0].penalty, 45);
        assert!(!is_recoverable("docker"));
    }
}
