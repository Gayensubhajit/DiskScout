//! Windows storage backend stub (Milestone 7).
//!
//! Present so the `crate::platform` interface already compiles on Windows.
//! Returns a graceful error that the UI surfaces instead of crashing.

use anyhow::Result;

use crate::storage::StorageInfo;

pub fn home_filesystem_info() -> Result<StorageInfo> {
    anyhow::bail!("Windows storage backend not implemented yet (Milestone 7)")
}
