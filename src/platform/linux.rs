//! Linux storage backend (Milestone 2).
//!
//! Uses the `sysinfo` crate, which reads `/proc` mounts and `statvfs`
//! natively. Never shells out to `df`.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use sysinfo::Disks;

use crate::storage::StorageInfo;

/// Home directory for mount matching and as the M3 scan root:
/// `$HOME`, else `/`.
fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Longest mount-point prefix of `path`, i.e. the filesystem containing it.
fn best_mount<'a>(disks: &'a Disks, path: &Path) -> Option<&'a sysinfo::Disk> {
    let mut best: Option<&'a sysinfo::Disk> = None;
    let mut best_len = 0usize;
    for disk in disks.list() {
        let mount = disk.mount_point();
        if path.starts_with(mount) {
            let len = mount.as_os_str().len();
            if len > best_len {
                best_len = len;
                best = Some(disk);
            }
        }
    }
    best
}

/// Scan root for Milestone 3: the user's home directory.
/// Starts at `$HOME`, never `/`, so `/proc`, `/sys`, `/dev` and other
/// mounts are out of scope until the mount-discovery layer (M8).
pub fn default_scan_root() -> Result<PathBuf> {
    Ok(home_dir())
}

/// Known Trash locations, relative to the scan root (M4). Freedesktop
/// Trash for the owning user lives under `~/.local/share/Trash`.
pub fn trash_relpaths() -> &'static [&'static str] {
    &[".local/share/Trash"]
}

/// Known application-storage locations, relative to the scan root (M4).
/// Conservative on purpose: only locations that are unambiguously
/// application data. `~/.local/share/applications` holds tiny `.desktop`
/// launchers (kept for ownership truth, not size); the real weight comes
/// from Flatpak/Steam payloads. `~/.config` is deliberately absent: it
/// mixes app config with user data and falls into Other until M7 can
/// attribute it reliably. Never includes all of `~/.local`.
pub fn app_storage_relpaths() -> &'static [&'static str] {
    &[
        ".local/share/flatpak",
        ".var/app",
        ".local/share/Steam",
        ".local/share/applications",
    ]
}

/// Known user cache/temp locations, relative to the scan root (M4).
/// Only locations that are unambiguously regenerable cache. Arbitrary
/// hidden directories are NOT temporary.
pub fn cache_relpaths() -> &'static [&'static str] {
    &[".cache"]
}

/// Snapshot of the filesystem containing the user's home directory.
pub fn home_filesystem_info() -> Result<StorageInfo> {
    let home = home_dir();
    // Canonicalize so symlinked `$HOME` (e.g. `/home` -> elsewhere) still
    // matches the real mount point. Fall back to the raw path otherwise.
    let probe = std::fs::canonicalize(&home).unwrap_or_else(|_| home.clone());

    let disks = Disks::new_with_refreshed_list();
    let disk = best_mount(&disks, &probe)
        .with_context(|| format!("no filesystem found containing {}", probe.to_string_lossy()))?;

    let total_bytes = disk.total_space();
    let available_bytes = disk.available_space();
    let used_bytes = total_bytes.saturating_sub(available_bytes);
    let usage_ratio = if total_bytes > 0 {
        (used_bytes as f64 / total_bytes as f64).clamp(0.0, 1.0) as f32
    } else {
        0.0
    };

    Ok(StorageInfo {
        device: disk.name().to_string_lossy().into_owned(),
        mount_point: disk.mount_point().to_string_lossy().into_owned(),
        filesystem: disk.file_system().to_string_lossy().into_owned(),
        total_bytes,
        used_bytes,
        available_bytes,
        usage_ratio,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_a_filesystem_for_home() {
        let info = home_filesystem_info().expect("must detect home filesystem");
        assert!(info.total_bytes > 0, "total capacity must be positive");
        assert!(
            info.used_bytes + info.available_bytes <= info.total_bytes + 1,
            "used + available must not exceed total"
        );
    }
}
