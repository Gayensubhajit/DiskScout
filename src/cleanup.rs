//! Storage cleanup actions and inspection (Milestone 6).
//!
//! Provides deterministic, safe cleanup for XDG Trash, user thumbnail caches,
//! and Btrfs filesystem status inspection.

use std::fs;
use std::path::Path;
use std::process::Command;

/// Measure total bytes in `$HOME/.local/share/Trash`.
pub fn query_trash_bytes(home: &Path) -> u64 {
    let trash_files = home.join(".local/share/Trash/files");
    measure_dir(&trash_files)
}

/// Measure total bytes in `$HOME/.cache/thumbnails`.
pub fn query_cache_bytes(home: &Path) -> u64 {
    let thumbs = home.join(".cache/thumbnails");
    measure_dir(&thumbs)
}

/// Recursively measure size of a directory.
fn measure_dir(dir: &Path) -> u64 {
    if !dir.exists() {
        return 0;
    }
    let mut total = 0u64;
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if let Ok(meta) = entry.metadata() {
                if meta.is_file() {
                    total = total.saturating_add(meta.len());
                } else if meta.is_dir() && !meta.is_symlink() {
                    total = total.saturating_add(measure_dir(&path));
                }
            }
        }
    }
    total
}

/// Empty XDG Trash safely by deleting contents of `files/` and `info/`.
pub fn empty_trash(home: &Path) -> std::io::Result<u64> {
    let trash_dir = home.join(".local/share/Trash");
    let files_dir = trash_dir.join("files");
    let info_dir = trash_dir.join("info");

    let bytes_reclaimed = measure_dir(&files_dir);

    if files_dir.exists() {
        fs::remove_dir_all(&files_dir)?;
        fs::create_dir_all(&files_dir)?;
    }
    if info_dir.exists() {
        fs::remove_dir_all(&info_dir)?;
        fs::create_dir_all(&info_dir)?;
    }

    Ok(bytes_reclaimed)
}

/// Clean expendable thumbnail caches in `$HOME/.cache/thumbnails`.
pub fn clean_thumbnail_cache(home: &Path) -> std::io::Result<u64> {
    let thumbs = home.join(".cache/thumbnails");
    let bytes_reclaimed = measure_dir(&thumbs);

    if thumbs.exists() {
        fs::remove_dir_all(&thumbs)?;
        fs::create_dir_all(&thumbs)?;
    }

    Ok(bytes_reclaimed)
}

/// Inspect Btrfs status on `/`.
pub fn query_btrfs_summary() -> (bool, String) {
    let output = Command::new("findmnt")
        .args(["-n", "-o", "FSTYPE", "/"])
        .output();

    let is_btrfs = match output {
        Ok(out) => String::from_utf8_lossy(&out.stdout).trim() == "btrfs",
        Err(_) => false,
    };

    if !is_btrfs {
        return (false, "Standard ext4 / non-btrfs filesystem".to_string());
    }

    // Try btrfs filesystem usage
    if let Ok(out) = Command::new("btrfs")
        .args(["filesystem", "usage", "-b", "/"])
        .output()
    {
        let text = String::from_utf8_lossy(&out.stdout);
        // Look for Device allocated and Device unallocated
        let mut allocated = "Unknown".to_string();
        let mut unallocated = "Unknown".to_string();
        for line in text.lines() {
            let l = line.trim();
            if l.starts_with("Device allocated:") {
                allocated = l.replace("Device allocated:", "").trim().to_string();
            } else if l.starts_with("Device unallocated:") {
                unallocated = l.replace("Device unallocated:", "").trim().to_string();
            }
        }
        return (
            true,
            format!(
                "Btrfs detected: Allocated: {} · Unallocated: {}",
                allocated, unallocated
            ),
        );
    }

    (true, "Btrfs filesystem active".to_string())
}

/// Open a path in the user's native file manager.
#[allow(dead_code)]
pub fn open_path(path_str: &str) -> std::io::Result<()> {
    Command::new("xdg-open").arg(path_str).spawn()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn trash_empty_flow() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let files = home.join(".local/share/Trash/files");
        let info = home.join(".local/share/Trash/info");
        fs::create_dir_all(&files).unwrap();
        fs::create_dir_all(&info).unwrap();
        fs::write(files.join("test.txt"), b"12345").unwrap();
        fs::write(info.join("test.txt.trashinfo"), b"metadata").unwrap();

        let before = query_trash_bytes(home);
        assert_eq!(before, 5);

        let reclaimed = empty_trash(home).unwrap();
        assert_eq!(reclaimed, 5);
        assert_eq!(query_trash_bytes(home), 0);
        assert!(files.exists());
        assert!(info.exists());
    }

    #[test]
    fn cache_clean_flow() {
        let dir = tempdir().unwrap();
        let home = dir.path();
        let thumbs = home.join(".cache/thumbnails");
        fs::create_dir_all(&thumbs).unwrap();
        fs::write(thumbs.join("thumb.png"), b"imagebytes").unwrap();

        let before = query_cache_bytes(home);
        assert_eq!(before, 10);

        let reclaimed = clean_thumbnail_cache(home).unwrap();
        assert_eq!(reclaimed, 10);
        assert_eq!(query_cache_bytes(home), 0);
        assert!(thumbs.exists());
    }
}
