//! System storage measurement for the Linux filesystem outside `$HOME`.
//!
//! Runs a fast recursive measurement of well-known system directories
//! (`/usr`, `/var`, `/opt`, `/boot`, `/etc`, etc.) on a background thread.
//!
//! ## Design constraints & boundaries
//! - Mount/Subvolume Boundary Awareness: Uses `st_dev` (`MetadataExt::dev`)
//!   to strictly enforce that traversal stays on the same filesystem/subvolume device
//!   as the root directory. Subdirectories that mount a separate Btrfs subvolume
//!   (e.g. `/var/cache`, `/var/log`, `/var/tmp`) are NOT traversed.
//! - Sparse File Allocation: Measures physical disk blocks (`st_blocks * 512`)
//!   rather than logical unallocated holes (avoiding inflation from virtual machine
//!   disk images, database sparse files, etc.).
//! - Snapshot Isolation: Does not count snapshot directories (`/.snapshots`)
//!   as ordinary live system files.
//! - Never scans pseudo-filesystems: `/proc`, `/sys`, `/dev`, `/run`.
//! - Never rescans `$HOME` (already covered by the main scan).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[cfg(unix)]
use std::os::unix::fs::MetadataExt;

/// Measured size for one system directory.
#[derive(Debug, Clone)]
pub struct SystemDirSize {
    /// Absolute path that was measured.
    pub path: PathBuf,
    /// Human-readable label for the UI.
    #[allow(dead_code)]
    pub label: &'static str,
    /// Detailed description for drilldown.
    pub description: &'static str,
    /// Recursive byte total (best-effort; permission errors are skipped).
    pub bytes: u64,
    /// Number of files discovered in this tree.
    pub files: u64,
    /// Sub-contributors (e.g. for /var breakdown: virtual machines, application state, package data, etc.).
    pub sub_dirs: Vec<SystemDirSize>,
}

/// Aggregate result of the system measurement pass.
#[derive(Debug, Clone, Default)]
pub struct SystemMeasurement {
    /// Total bytes across all measured system directories.
    pub total_bytes: u64,
    /// Per-directory breakdown (non-empty entries only).
    pub dirs: Vec<SystemDirSize>,
    /// Number of entries skipped due to permissions or missing paths.
    pub skipped: usize,
    /// In-memory directory tree of system directories for the explorer.
    pub file_tree: crate::scan::FileTree,
}

/// Recursive directory statistics (bytes and file count).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DirStats {
    pub bytes: u64,
    pub files: u64,
}

/// Shared store for the background measurement result.
pub type SystemStore = Arc<Mutex<Option<SystemMeasurement>>>;

/// System directories to measure with friendly human labels and descriptions.
/// Note: Btrfs snapshot directories are deliberately omitted from live system totals.
static SYSTEM_DIRS: &[(&str, &str, &str)] = &[
    ("/usr", "System software", "Core Linux files"),
    (
        "/var",
        "System & application data",
        "Services, databases, logs and state",
    ),
    (
        "/opt",
        "Optional software",
        "Software installed outside the main system",
    ),
    ("/boot", "Boot files", "Kernels and boot files"),
    ("/etc", "Configuration", "System-wide settings"),
    ("/root", "Superuser storage", "Administrator files"),
    ("/srv", "Service data", "Data for system and web services"),
];

/// Pseudo-filesystems and virtual directories — never scanned.
static EXCLUDE_PREFIXES: &[&str] = &["/proc", "/sys", "/dev", "/run"];

/// Measure the system directories on the calling thread (blocking).
/// Intended to be called from a background worker thread.
pub fn measure_system(home: &Path) -> SystemMeasurement {
    let all_dirs: Vec<(&Path, &'static str, &'static str)> = SYSTEM_DIRS
        .iter()
        .map(|(p, l, d)| (Path::new(*p), *l, *d))
        .collect();
    measure_custom_dirs(home, &all_dirs)
}

/// Measure specific directories while respecting home, pseudofs, and mount boundaries.
pub fn measure_custom_dirs(
    home: &Path,
    dirs: &[(&Path, &'static str, &'static str)],
) -> SystemMeasurement {
    let mut result = SystemMeasurement::default();

    for &(path, label, description) in dirs {
        // Skip if it is inside $HOME (home scan covers it already)
        if path.starts_with(home) {
            continue;
        }
        // Skip pseudo-filesystems
        if EXCLUDE_PREFIXES.iter().any(|ex| path.starts_with(ex)) {
            continue;
        }
        // Skip if the directory does not exist or is not readable
        if !path.is_dir() {
            result.skipped += 1;
            continue;
        }
        match dir_stats_with_tree(path, &mut result.file_tree) {
            Ok((stats, _dir_id)) => {
                if stats.bytes > 0 {
                    result.total_bytes += stats.bytes;
                    let sub_dirs = if path == Path::new("/var") {
                        classify_var_subdirs(path, &stats, &result.file_tree)
                    } else {
                        Vec::new()
                    };
                    result.dirs.push(SystemDirSize {
                        path: path.to_path_buf(),
                        label,
                        description,
                        bytes: stats.bytes,
                        files: stats.files,
                        sub_dirs,
                    });
                }
            }
            Err(_) => {
                result.skipped += 1;
            }
        }
    }

    result
}

/// Deconstruct /var into non-overlapping functional sub-contributors.
/// All children are already indexed in `file_tree` by `dir_stats_with_tree`.
pub fn classify_var_subdirs(
    var_path: &Path,
    total_stats: &DirStats,
    file_tree: &crate::scan::FileTree,
) -> Vec<SystemDirSize> {
    let mut sub_dirs = Vec::new();
    let mut claimed_bytes: u64 = 0;
    let mut claimed_files: u64 = 0;

    let functional_groups: &[(&[&str], &'static str, &'static str)] = &[
        (
            &["/var/lib/libvirt", "/var/lib/docker", "/var/lib/containers"],
            "Virtual machines & containers",
            "Virtual machine disks, images and container storage",
        ),
        (
            &["/var/lib/flatpak", "/var/lib/snapd"],
            "Application state",
            "Application runtimes and service state",
        ),
        (
            &["/var/lib/pacman", "/var/lib/dpkg", "/var/lib/rpm"],
            "Package data",
            "Package manager database and metadata",
        ),
        (
            &["/var/cache/pacman", "/var/cache/apt"],
            "Package cache",
            "Package download caches",
        ),
        (
            &["/var/log"],
            "Logs & journals",
            "System logs and journal data",
        ),
        (
            &["/var/lib/systemd"],
            "System services",
            "Service state and system databases",
        ),
    ];

    for &(paths, label, desc) in functional_groups {
        let mut group_bytes: u64 = 0;
        let mut group_files: u64 = 0;
        let mut group_path = None;

        for &p_str in paths {
            let p = Path::new(p_str);
            if let Some(dir_id) = file_tree.find_dir(p) {
                if let Some(node) = file_tree.get_dir(dir_id) {
                    if node.bytes > 0 {
                        group_bytes = group_bytes.saturating_add(node.bytes);
                        group_files = group_files.saturating_add(node.file_count);
                        if group_path.is_none() {
                            group_path = Some(p.to_path_buf());
                        }
                    }
                }
            }
        }

        if group_bytes > 0 {
            claimed_bytes = claimed_bytes.saturating_add(group_bytes);
            claimed_files = claimed_files.saturating_add(group_files);
            sub_dirs.push(SystemDirSize {
                path: group_path.unwrap_or_else(|| var_path.to_path_buf()),
                label,
                description: desc,
                bytes: group_bytes,
                files: group_files,
                sub_dirs: Vec::new(),
            });
        }
    }

    let remainder_bytes = total_stats.bytes.saturating_sub(claimed_bytes);
    let remainder_files = total_stats.files.saturating_sub(claimed_files);

    if remainder_bytes > 0 || remainder_files > 0 {
        sub_dirs.push(SystemDirSize {
            path: var_path.to_path_buf(),
            label: "Other system data",
            description: "Remaining system and service files",
            bytes: remainder_bytes,
            files: remainder_files,
            sub_dirs: Vec::new(),
        });
    }

    sub_dirs.sort_by_key(|s| std::cmp::Reverse(s.bytes));
    sub_dirs
}

/// Recursive directory statistics respecting mount/subvolume boundaries and sparse files.
/// Builds directory nodes into the provided `file_tree` for lazy explorer navigation.
pub fn dir_stats_with_tree(
    dir: &Path,
    file_tree: &mut crate::scan::FileTree,
) -> std::io::Result<(DirStats, Option<u32>)> {
    let root_meta = std::fs::metadata(dir)?;
    #[cfg(unix)]
    let root_dev = root_meta.dev();
    #[cfg(not(unix))]
    let root_dev = 0u64;

    dir_stats_recursive(dir, root_dev, file_tree)
}

/// Recursive directory statistics respecting mount/subvolume boundaries and sparse files.
/// Subdirectories located on a different device (`dev != root_dev`) are strictly skipped.
pub fn dir_stats(dir: &Path) -> std::io::Result<DirStats> {
    let mut tree = crate::scan::FileTree::default();
    let (stats, _) = dir_stats_with_tree(dir, &mut tree)?;
    Ok(stats)
}

/// Recursive traversal helper that records allocated file bytes and directory nodes.
fn dir_stats_recursive(
    dir: &Path,
    root_dev: u64,
    file_tree: &mut crate::scan::FileTree,
) -> std::io::Result<(DirStats, Option<u32>)> {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(err) if err.kind() == std::io::ErrorKind::PermissionDenied => {
            return Ok((DirStats::default(), None));
        }
        Err(err) => return Err(err),
    };

    let mut children: Vec<crate::scan::FileEntry> = Vec::new();
    let mut dir_bytes: u64 = 0;
    let mut dir_files: u64 = 0;

    for entry in entries.flatten() {
        let path = entry.path();
        if EXCLUDE_PREFIXES.iter().any(|ex| path == Path::new(ex)) {
            continue;
        }
        let meta = match entry.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };

        #[cfg(unix)]
        if meta.dev() != root_dev {
            // Mount boundary or separate Btrfs subvolume (e.g. /var/cache, /var/log, /var/tmp)
            continue;
        }

        let name = entry.file_name().to_string_lossy().into_owned();

        if meta.is_dir() {
            if !meta.is_symlink() {
                if let Ok((sub_stats, child_dir_id)) =
                    dir_stats_recursive(&path, root_dev, file_tree)
                {
                    dir_bytes = dir_bytes.saturating_add(sub_stats.bytes);
                    dir_files = dir_files.saturating_add(sub_stats.files);
                    children.push(crate::scan::FileEntry {
                        name,
                        is_dir: true,
                        bytes: sub_stats.bytes,
                        file_count: sub_stats.files,
                        dir_id: child_dir_id,
                    });
                }
            }
        } else {
            let alloc = crate::platform::allocated_file_bytes(&meta);
            dir_bytes = dir_bytes.saturating_add(alloc);
            dir_files += 1;
            children.push(crate::scan::FileEntry {
                name,
                is_dir: false,
                bytes: alloc,
                file_count: 1,
                dir_id: None,
            });
        }
    }

    children.sort_by_key(|c| std::cmp::Reverse(c.bytes));

    let dir_id = file_tree.directories.len() as u32;
    file_tree.path_to_dir.insert(dir.to_path_buf(), dir_id);
    let dir_name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dir.to_string_lossy().into_owned());

    file_tree.directories.push(crate::scan::DirectoryNode {
        path: dir.to_path_buf(),
        name: dir_name,
        bytes: dir_bytes,
        file_count: dir_files,
        parent_dir: None,
        children,
    });

    Ok((
        DirStats {
            bytes: dir_bytes,
            files: dir_files,
        },
        Some(dir_id),
    ))
}

/// Recursive directory size (convenience wrapper for `dir_stats`).
#[allow(dead_code)]
pub fn dir_size_bytes(dir: &Path) -> std::io::Result<u64> {
    dir_stats(dir).map(|s| s.bytes)
}

/// Spawn background system measurement. Calls `done_cb` when complete.
pub fn spawn_system_measurement(
    home: PathBuf,
    store: SystemStore,
    done_cb: impl FnOnce(SystemMeasurement) + Send + 'static,
) {
    std::thread::Builder::new()
        .name("diskscout-sysinfo".to_string())
        .spawn(move || {
            let t = std::time::Instant::now();
            let result = measure_system(&home);
            eprintln!(
                "[diskscout perf] system measurement: {:.1}ms ({} dirs, {} bytes total, {} skipped)",
                t.elapsed().as_secs_f64() * 1000.0,
                result.dirs.len(),
                result.total_bytes,
                result.skipped,
            );
            {
                let mut guard = store.lock().expect("system store poisoned");
                *guard = Some(result.clone());
            }
            done_cb(result);
        })
        .expect("failed to spawn sysinfo thread");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_dir_size_bytes_tmp() {
        let size = dir_size_bytes(Path::new("/tmp"));
        assert!(size.is_ok(), "should be able to read /tmp");
    }

    #[test]
    fn test_dir_size_bytes_nonexistent() {
        let size = dir_size_bytes(Path::new("/nonexistent_diskscout_test_dir_xyz"));
        assert!(size.is_err(), "non-existent dir should return error");
    }

    #[test]
    fn test_measure_system_does_not_include_home() {
        let home =
            PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| "/home/user".to_string()));
        let result = measure_system(&home);
        for dir in &result.dirs {
            assert!(
                !dir.path.starts_with(&home),
                "system measurement must not include $HOME: {:?}",
                dir.path
            );
        }
    }

    #[test]
    fn test_measure_system_no_pseudofs() {
        let home = PathBuf::from("/home/testuser");
        let result = measure_system(&home);
        for dir in &result.dirs {
            for ex in EXCLUDE_PREFIXES {
                assert!(
                    !dir.path.starts_with(ex),
                    "system measurement must not include pseudo-fs {:?}: {:?}",
                    ex,
                    dir.path
                );
            }
        }
    }

    #[test]
    fn test_measure_custom_dirs_boundary_and_exclusion() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let home = root.join("home/user");
        let usr = root.join("usr");
        let proc = root.join("proc");

        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&usr).unwrap();
        std::fs::create_dir_all(&proc).unwrap();

        std::fs::write(home.join("data.txt"), "hello").unwrap();
        std::fs::write(usr.join("bin.bin"), "system binary content").unwrap();
        std::fs::write(proc.join("fake_stat"), "proc").unwrap();

        // Pass home, usr, proc, and pseudo-fs path
        let pseudo = Path::new("/proc/sys");
        let dirs: Vec<(&Path, &str, &str)> = vec![
            (home.as_path(), "User Home", "Home description"),
            (usr.as_path(), "System Usr", "Usr description"),
            (pseudo, "Kernel Proc", "Proc description"),
        ];

        let meas = measure_custom_dirs(&home, &dirs);
        // Home must be excluded because it starts_with home
        assert!(!meas.dirs.iter().any(|d| d.path == home));
        // Pseudo must be excluded
        assert!(!meas.dirs.iter().any(|d| d.path == pseudo));
        // Usr must be measured
        assert!(meas.dirs.iter().any(|d| d.path == usr));
        assert!(meas.total_bytes > 0);
        let usr_dir = meas.dirs.iter().find(|d| d.path == usr).unwrap();
        assert_eq!(usr_dir.files, 1);
        assert_eq!(usr_dir.label, "System Usr");
        assert_eq!(usr_dir.description, "Usr description");
    }

    #[test]
    fn test_mount_boundary_subvolume_isolation() {
        // When dir_stats encounters entries on a different st_dev, it must not descend.
        // /var on Btrfs has mounts /var/cache, /var/log, /var/tmp with different st_dev.
        // If /var exists, dir_stats must succeed without crashing and without counting subvolumes.
        let var_path = Path::new("/var");
        if var_path.is_dir() {
            let stats = dir_stats(var_path).expect("should measure /var");
            assert!(stats.bytes > 0);
            assert!(stats.files > 0);
        }
    }

    #[test]
    fn test_dir_stats_measures_sparse_file() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let sparse_path = root.join("vm_disk.qcow2");
        let f = std::fs::File::create(&sparse_path).unwrap();
        f.set_len(100 * 1024 * 1024).unwrap(); // 100 MB sparse file

        let normal_path = root.join("file.txt");
        std::fs::write(&normal_path, b"12345").unwrap();

        let stats = dir_stats(root).unwrap();
        assert_eq!(stats.files, 2);
        #[cfg(unix)]
        {
            assert_eq!(
                stats.bytes, 5,
                "100MB sparse file with 0 blocks must not inflate dir stats"
            );
        }
    }

    #[test]
    fn test_system_explorer_indexes_children() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        let usr = root.join("usr");
        let usr_bin = usr.join("bin");
        let usr_lib = usr.join("lib");
        std::fs::create_dir_all(&usr_bin).unwrap();
        std::fs::create_dir_all(&usr_lib).unwrap();
        std::fs::write(usr_bin.join("sh"), b"echo hi").unwrap();
        std::fs::write(usr_lib.join("libc.so"), b"binary bytes here").unwrap();

        let dirs: Vec<(&Path, &str, &str)> =
            vec![(usr.as_path(), "System software", "Core Linux files")];

        let meas = measure_custom_dirs(Path::new("/home/user"), &dirs);
        assert_eq!(meas.dirs.len(), 1);
        assert_eq!(meas.dirs[0].label, "System software");

        // Verify that usr, usr/bin, and usr/lib are all in file_tree
        let usr_dir_id = meas.file_tree.find_dir(&usr);
        assert!(usr_dir_id.is_some(), "/usr must be indexed in file_tree");
        let usr_node = meas.file_tree.get_dir(usr_dir_id.unwrap()).unwrap();
        assert_eq!(
            usr_node.children.len(),
            2,
            "/usr must have 2 child directories"
        );

        // Verify child directories can be resolved
        assert!(meas.file_tree.find_dir(&usr_bin).is_some());
        assert!(meas.file_tree.find_dir(&usr_lib).is_some());
    }
    #[test]
    fn test_var_parent_child_classification_zero_overlap() {
        let tmp = tempfile::tempdir().unwrap();
        let var = tmp.path().join("var");
        let libvirt = var.join("lib/libvirt");
        let flatpak = var.join("lib/flatpak");
        let pacman = var.join("lib/pacman");
        let systemd = var.join("lib/systemd");
        let spool = var.join("spool");

        std::fs::create_dir_all(&libvirt).unwrap();
        std::fs::create_dir_all(&flatpak).unwrap();
        std::fs::create_dir_all(&pacman).unwrap();
        std::fs::create_dir_all(&systemd).unwrap();
        std::fs::create_dir_all(&spool).unwrap();

        std::fs::write(libvirt.join("vm.img"), vec![0u8; 4000]).unwrap();
        std::fs::write(flatpak.join("app.bin"), vec![0u8; 2000]).unwrap();
        std::fs::write(pacman.join("local.db"), vec![0u8; 1000]).unwrap();
        std::fs::write(systemd.join("state"), vec![0u8; 500]).unwrap();
        std::fs::write(spool.join("mail"), vec![0u8; 300]).unwrap();

        let mut file_tree = crate::scan::FileTree::default();
        let (stats, _) = dir_stats_with_tree(&var, &mut file_tree).unwrap();
        assert_eq!(stats.files, 5);

        // Map subdirs using classify_var_subdirs logic with our custom paths
        let sub_dirs = classify_var_subdirs(&var, &stats, &file_tree);
        // Sum of all sub_dirs bytes must equal total /var bytes
        let sub_bytes_sum: u64 = sub_dirs.iter().map(|s| s.bytes).sum();
        assert_eq!(
            sub_bytes_sum, stats.bytes,
            "sum of subdirs must equal total /var bytes"
        );
        let sub_files_sum: u64 = sub_dirs.iter().map(|s| s.files).sum();
        assert_eq!(
            sub_files_sum, stats.files,
            "sum of subdirs files must equal total /var files"
        );

        // The unclaimed remainder ("Other system data") must be present
        let remainder = sub_dirs
            .iter()
            .find(|s| s.label == "Other system data")
            .expect("Other system data must exist");
        assert!(
            remainder.bytes > 0,
            "Other system data must capture the unclaimed spool bytes"
        );

        // Invariant: parent /var size (stats.bytes) strictly accounts for every byte once
        assert_eq!(stats.bytes, sub_bytes_sum);
    }

    #[test]
    fn test_var_nonexistent_optional_directories_not_present() {
        let tmp = tempfile::tempdir().unwrap();
        let var = tmp.path().join("var");
        std::fs::create_dir_all(&var).unwrap();
        std::fs::write(var.join("log.txt"), b"some logs").unwrap();

        let mut file_tree = crate::scan::FileTree::default();
        let (stats, _) = dir_stats_with_tree(&var, &mut file_tree).unwrap();

        let sub_dirs = classify_var_subdirs(&var, &stats, &file_tree);
        // Nonexistent categories like libvirt/docker must NOT appear with 0 bytes
        assert!(
            !sub_dirs
                .iter()
                .any(|s| s.label == "Virtual machines & containers")
        );
        assert!(!sub_dirs.iter().any(|s| s.label == "Package data"));
        assert_eq!(sub_dirs.len(), 1);
        assert_eq!(sub_dirs[0].label, "Other system data");
        assert_eq!(sub_dirs[0].bytes, stats.bytes);
    }
}
