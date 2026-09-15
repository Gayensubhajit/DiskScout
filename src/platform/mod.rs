//! OS-specific storage backends behind one interface.
//!
//! Linux is implemented (Milestone 2). Windows is a stub for Milestone 7.

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(target_os = "linux")]
pub use linux::{
    app_storage_relpaths, cache_relpaths, default_scan_root, home_filesystem_info, trash_relpaths,
};

#[cfg(target_os = "windows")]
pub use windows::{app_storage_relpaths, cache_relpaths, default_scan_root, home_filesystem_info};

/// Fallback for untested platforms: surface a clear error to the UI
/// instead of guessing.
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn home_filesystem_info() -> anyhow::Result<crate::storage::StorageInfo> {
    anyhow::bail!("storage backend not implemented for this OS")
}

/// Fallback scan root for untested platforms.
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn default_scan_root() -> anyhow::Result<std::path::PathBuf> {
    anyhow::bail!("scanner backend not implemented for this OS")
}

/// Fallback M4 location tables for untested platforms.
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn trash_relpaths() -> &'static [&'static str] {
    &[]
}

/// See [`trash_relpaths`].
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn app_storage_relpaths() -> &'static [&'static str] {
    &[]
}

/// See [`trash_relpaths`].
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn cache_relpaths() -> &'static [&'static str] {
    &[]
}
