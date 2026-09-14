//! Cross-platform storage information.
//!
//! The Slint UI must never talk to the OS directly. It receives a plain
//! [`StorageInfo`] value produced by the platform backend selected in
//! [`crate::platform`]. Linux is implemented natively (no `df` parsing);
//! Windows is a stub until Milestone 7.

use anyhow::Result;

/// Point-in-time capacity snapshot for one filesystem.
#[derive(Debug, Clone)]
pub struct StorageInfo {
    /// OS device, e.g. `/dev/nvme0n1p5`. May be empty if unknown.
    pub device: String,
    /// Mount point, e.g. `/` or `/home`.
    pub mount_point: String,
    /// Filesystem type, e.g. `btrfs`, `ext4`, `ntfs`. Consumed by
    /// Milestone 6 (Btrfs/Snapper intelligence); collected already so the
    /// backend interface does not churn.
    #[allow(dead_code)]
    pub filesystem: String,
    /// Total capacity in bytes.
    pub total_bytes: u64,
    /// Used bytes (`total - available`).
    pub used_bytes: u64,
    /// Available bytes for unprivileged use.
    pub available_bytes: u64,
    /// `used / total` in `0.0..=1.0`. `0.0` when total is 0.
    pub usage_ratio: f32,
}

impl StorageInfo {
    /// Placeholder shown when the backend fails and the UI must still render.
    pub fn unavailable() -> Self {
        Self {
            device: String::new(),
            mount_point: String::new(),
            filesystem: String::new(),
            total_bytes: 0,
            used_bytes: 0,
            available_bytes: 0,
            usage_ratio: 0.0,
        }
    }

    /// Short drive label, e.g. `/dev/nvme0n1p5`. Falls back to mount point.
    pub fn display_name(&self) -> String {
        if !self.device.is_empty() {
            self.device.clone()
        } else if !self.mount_point.is_empty() {
            self.mount_point.clone()
        } else {
            "Local Disk".to_string()
        }
    }

    /// One-line health summary for the status row.
    pub fn health_text(&self) -> &'static str {
        if self.total_bytes == 0 {
            "Storage information unavailable"
        } else if self.usage_ratio >= 0.9 {
            "Storage is almost full"
        } else if self.usage_ratio >= 0.8 {
            "Storage is getting full"
        } else {
            "Your storage is healthy"
        }
    }
}

/// Query the filesystem containing the user's home directory.
///
/// Dispatches to the platform backend; never shells out to `df`.
pub fn query_home_filesystem() -> Result<StorageInfo> {
    crate::platform::home_filesystem_info()
}

/// `2048 -> "2.0 KB"`, `341 GiB -> "341.0 GB"`. Binary units, decimal labels
/// to match the style of `df` output shown in the dashboard.
pub fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KB", "MB", "GB", "TB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit + 1 < UNITS.len() {
        value /= 1024.0;
        unit += 1;
    }
    if unit == 0 {
        format!("{} {}", bytes, UNITS[unit])
    } else {
        format!("{:.1} {}", value, UNITS[unit])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_bytes() {
        assert_eq!(format_bytes(0), "0 B");
        assert_eq!(format_bytes(512), "512 B");
        assert_eq!(format_bytes(2048), "2.0 KB");
        assert_eq!(format_bytes(1024 * 1024 * 1024 * 341), "341.0 GB");
    }

    #[test]
    fn health_thresholds() {
        let base = StorageInfo {
            device: String::new(),
            mount_point: String::new(),
            filesystem: String::new(),
            total_bytes: 100,
            used_bytes: 0,
            available_bytes: 100,
            usage_ratio: 0.5,
        };
        assert_eq!(base.health_text(), "Your storage is healthy");
        let full = StorageInfo {
            usage_ratio: 0.95,
            ..base.clone()
        };
        assert_eq!(full.health_text(), "Storage is almost full");
    }
}
