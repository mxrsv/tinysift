//! Pure helpers shared across commands. Side-effect free; safe to unit test.

/// Format a byte count as a human-readable string (B, KB, MB, GB, TB).
/// Uses 1024 as the base. Values < 1024 keep integer formatting; everything
/// larger uses two decimal places.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{:.2} {}", value, UNITS[unit])
    }
}

/// Format a duration in seconds as a coarse human string (e.g. "2d 3h 4m").
/// Returns the most relevant two units only.
pub fn format_duration(seconds: u64) -> String {
    let days = seconds / 86_400;
    let hours = (seconds % 86_400) / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let secs = seconds % 60;
    if days > 0 {
        format!("{}d {}h {}m", days, hours, minutes)
    } else if hours > 0 {
        format!("{}h {}m", hours, minutes)
    } else if minutes > 0 {
        format!("{}m {}s", minutes, secs)
    } else {
        format!("{}s", secs)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn format_bytes_handles_each_unit_threshold() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(1024), "1.00 KB");
        assert_eq!(format_bytes(1536), "1.50 KB");
        assert_eq!(format_bytes(1024 * 1024), "1.00 MB");
        assert_eq!(format_bytes(1024u64.pow(3)), "1.00 GB");
        assert_eq!(format_bytes(1024u64.pow(4)), "1.00 TB");
    }

    #[test]
    fn format_bytes_caps_at_terabytes() {
        // Petabyte-range values should still be printed as TB, not overflow.
        let pb = 1024u64.pow(5);
        assert!(format_bytes(pb).ends_with("TB"));
    }

    #[test]
    fn format_duration_uses_largest_unit() {
        assert_eq!(format_duration(0), "0s");
        assert_eq!(format_duration(45), "45s");
        assert_eq!(format_duration(60), "1m 0s");
        assert_eq!(format_duration(3_600), "1h 0m");
        assert_eq!(format_duration(86_400), "1d 0h 0m");
        assert_eq!(format_duration(90_061), "1d 1h 1m");
    }
}
