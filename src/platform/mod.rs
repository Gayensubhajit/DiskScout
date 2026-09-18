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

/// Returns the physical allocated disk space in bytes for a file.
///
/// Rules:
/// - Regular file allocation uses `st_blocks * 512` on Unix.
/// - Capped against `st_size` (`meta.len()`) where appropriate to handle small files.
/// - Sparse holes must not inflate storage (blocks will be less than apparent size).
/// - Symlinks are not followed/dereferenced (returns 0).
/// - Non-Unix builds retain the platform fallback (`meta.len()`).
pub fn allocated_file_bytes(meta: &std::fs::Metadata) -> u64 {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        if meta.file_type().is_symlink() {
            return 0;
        }
        meta.blocks().saturating_mul(512).min(meta.len())
    }
    #[cfg(not(unix))]
    {
        meta.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use std::io::Write;

    #[test]
    fn test_allocated_file_bytes_normal_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("normal.txt");
        let mut f = File::create(&path).unwrap();
        f.write_all(b"Hello world! This is a test file with some bytes.")
            .unwrap();
        let meta = std::fs::symlink_metadata(&path).unwrap();
        let alloc = allocated_file_bytes(&meta);
        assert_eq!(alloc, meta.len());
        assert!(alloc > 0);
    }

    #[test]
    fn test_allocated_file_bytes_zero_byte() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("zero.txt");
        File::create(&path).unwrap();
        let meta = std::fs::symlink_metadata(&path).unwrap();
        let alloc = allocated_file_bytes(&meta);
        assert_eq!(alloc, 0);
    }

    #[test]
    fn test_allocated_file_bytes_sparse_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("sparse.bin");
        let f = File::create(&path).unwrap();
        // 10 MB sparse file without writing blocks
        f.set_len(10 * 1024 * 1024).unwrap();
        let meta = std::fs::symlink_metadata(&path).unwrap();
        let alloc = allocated_file_bytes(&meta);
        #[cfg(unix)]
        {
            assert!(alloc < 10 * 1024 * 1024);
            assert_eq!(alloc, 0);
        }
        #[cfg(not(unix))]
        {
            assert_eq!(alloc, 10 * 1024 * 1024);
        }
    }

    #[test]
    fn test_allocated_file_bytes_symlink_not_dereferenced() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("target.txt");
        std::fs::write(&target, b"some content in target file").unwrap();

        let link = dir.path().join("link.txt");
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink(&target, &link).unwrap();
            let meta = std::fs::symlink_metadata(&link).unwrap();
            let alloc = allocated_file_bytes(&meta);
            assert_eq!(alloc, 0, "symlinks must return 0 allocated bytes");
        }
    }
}
