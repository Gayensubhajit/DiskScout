//! OS-specific storage backends behind one interface.
//!
//! Linux is implemented (Milestone 2). Windows is a stub for Milestone 7.

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "windows")]
pub mod windows;

#[cfg(target_os = "linux")]
pub use linux::home_filesystem_info;

#[cfg(target_os = "windows")]
pub use windows::home_filesystem_info;

/// Fallback for untested platforms: surface a clear error to the UI
/// instead of guessing.
#[cfg(not(any(target_os = "linux", target_os = "windows")))]
pub fn home_filesystem_info() -> anyhow::Result<crate::storage::StorageInfo> {
    anyhow::bail!("storage backend not implemented for this OS")
}
