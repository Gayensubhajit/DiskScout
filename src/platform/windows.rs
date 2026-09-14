//! Windows storage backend stub (Milestone 7).
//!
//! Present so the `crate::platform` interface already compiles on Windows.
//! Returns a graceful error that the UI surfaces instead of crashing.

use std::path::PathBuf;

use anyhow::Result;

use crate::storage::StorageInfo;

pub fn home_filesystem_info() -> Result<StorageInfo> {
    anyhow::bail!("Windows storage backend not implemented yet (Milestone 7)")
}

/// Scan root for Milestone 3: `%USERPROFILE%`. The core walker in
/// `crate::scan` is portable; only capacity stats await Milestone 7/9.
pub fn default_scan_root() -> Result<PathBuf> {
    std::env::var_os("USERPROFILE")
        .map(PathBuf::from)
        .filter(|p| !p.as_os_str().is_empty())
        .ok_or_else(|| anyhow::anyhow!("%USERPROFILE% is not set"))
}
