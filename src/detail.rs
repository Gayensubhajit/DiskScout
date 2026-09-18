//! Category detail model (Milestone 5, read-only).
//!
//! Answers *"why is this category this large?"* from M4 provenance.
//! No rescans, no filesystem I/O: everything derives from the in-memory
//! [`StorageClassification`]. Nothing here deletes, moves, or otherwise
//! mutates user data; cleanup stays a future milestone.
//!
//! Human-label policy: well-known locations get friendly names and descriptions
//! ("Flatpak applications", "Installed Flatpak applications");
//! everything else falls back to its plain directory/file name.
//! Raw paths stay in Rust and are never exposed as raw strings in the UI.

use std::path::{Path, PathBuf};

use crate::classify::{Category, ContributionScope, StorageClassification};

/// One breakdown row: an aggregated contribution, never individual files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetailItem {
    pub path: PathBuf,
    pub label: String,
    pub description: String,
    pub bytes: u64,
    pub files: u64,
    pub icon_name: String,
    pub scope: ContributionScope,
    pub sub_items: Vec<DetailItem>,
}

/// Detail view model for one category.
#[derive(Debug, Clone)]
pub struct CategoryDetail {
    pub total_bytes: u64,
    pub rows: Vec<DetailItem>,
}

/// Build the detail model. Zero-byte categories yield zero rows; the UI
/// shows its empty state from that. Pure function of classified data.
/// Always sorts rows descending by size (largest contributors first).
pub fn detail_for(
    classification: &StorageClassification,
    home: &Path,
    category: Category,
) -> CategoryDetail {
    let total = classification.of(category);
    if total.bytes == 0 {
        return CategoryDetail {
            total_bytes: 0,
            rows: Vec::new(),
        };
    }

    // Specialized intelligent breakdown for Category::Other
    if category == Category::Other {
        let items: Vec<DetailItem> = total
            .contributions
            .iter()
            .map(|c| {
                let (label, description, icon_name) = detail_info(category, &c.path, home);
                DetailItem {
                    path: c.path.clone(),
                    label,
                    description,
                    bytes: c.bytes,
                    files: c.files,
                    icon_name,
                    scope: c.scope.clone(),
                    sub_items: Vec::new(),
                }
            })
            .collect();

        struct GroupAccum {
            label: &'static str,
            description: &'static str,
            icon_name: &'static str,
            bytes: u64,
            files: u64,
            primary_path: PathBuf,
            items: Vec<DetailItem>,
        }

        let mut groups: Vec<GroupAccum> = vec![
            GroupAccum {
                label: "Developer data",
                description: "Projects, build files and packages",
                icon_name: "folder",
                bytes: 0,
                files: 0,
                primary_path: home.to_path_buf(),
                items: Vec::new(),
            },
            GroupAccum {
                label: "Application data",
                description: "Application data and local storage",
                icon_name: "folder",
                bytes: 0,
                files: 0,
                primary_path: home.to_path_buf(),
                items: Vec::new(),
            },
            GroupAccum {
                label: "Browser data",
                description: "Browser profiles and cached data",
                icon_name: "folder",
                bytes: 0,
                files: 0,
                primary_path: home.to_path_buf(),
                items: Vec::new(),
            },
            GroupAccum {
                label: "Game data",
                description: "Game-related files",
                icon_name: "folder",
                bytes: 0,
                files: 0,
                primary_path: home.to_path_buf(),
                items: Vec::new(),
            },
            GroupAccum {
                label: "Cache data",
                description: "User cache and temporary data",
                icon_name: "temp",
                bytes: 0,
                files: 0,
                primary_path: home.to_path_buf(),
                items: Vec::new(),
            },
            GroupAccum {
                label: "Unclassified",
                description: "Files not yet assigned to another category",
                icon_name: "folder",
                bytes: 0,
                files: 0,
                primary_path: home.to_path_buf(),
                items: Vec::new(),
            },
        ];

        for item in items {
            let (group_label, _, _) = other_group_category(&item.path, home, &item.label);
            if let Some(group) = groups.iter_mut().find(|g| g.label == group_label) {
                if group.bytes == 0 {
                    group.primary_path = item.path.clone();
                }
                group.bytes = group.bytes.saturating_add(item.bytes);
                group.files = group.files.saturating_add(item.files);
                group.items.push(item);
            }
        }

        let mut rows: Vec<DetailItem> = groups
            .into_iter()
            .filter(|g| g.bytes > 0 || g.files > 0)
            .map(|mut g| {
                g.items.sort_by_key(|i| std::cmp::Reverse(i.bytes));
                let scope = ContributionScope::whole_subtree(g.primary_path.clone());
                DetailItem {
                    path: g.primary_path,
                    label: g.label.to_string(),
                    description: g.description.to_string(),
                    bytes: g.bytes,
                    files: g.files,
                    icon_name: g.icon_name.to_string(),
                    scope,
                    sub_items: g.items,
                }
            })
            .collect();

        rows.sort_by_key(|r| std::cmp::Reverse(r.bytes));
        return CategoryDetail {
            total_bytes: total.bytes,
            rows,
        };
    }

    // Specialized breakdown for Category::System with /var sub-contributors
    if category == Category::System {
        let mut rows: Vec<DetailItem> = total
            .contributions
            .iter()
            .map(|c| {
                let (label, description, icon_name) = detail_info(category, &c.path, home);
                let mut sub_items: Vec<DetailItem> = c
                    .sub_contributions
                    .iter()
                    .map(|sub| {
                        let (sub_label, sub_desc, sub_icon) =
                            detail_info(category, &sub.path, home);
                        DetailItem {
                            path: sub.path.clone(),
                            label: sub_label,
                            description: sub_desc,
                            bytes: sub.bytes,
                            files: sub.files,
                            icon_name: sub_icon,
                            scope: sub.scope.clone(),
                            sub_items: Vec::new(),
                        }
                    })
                    .collect();
                sub_items.sort_by_key(|s| std::cmp::Reverse(s.bytes));
                DetailItem {
                    path: c.path.clone(),
                    label,
                    description,
                    bytes: c.bytes,
                    files: c.files,
                    icon_name,
                    scope: c.scope.clone(),
                    sub_items,
                }
            })
            .collect();

        rows.sort_by_key(|r| std::cmp::Reverse(r.bytes));
        return CategoryDetail {
            total_bytes: total.bytes,
            rows,
        };
    }

    // Default breakdown for all other categories
    let mut rows: Vec<DetailItem> = total
        .contributions
        .iter()
        .map(|c| {
            let (label, description, icon_name) = detail_info(category, &c.path, home);
            DetailItem {
                path: c.path.clone(),
                label,
                description,
                bytes: c.bytes,
                files: c.files,
                icon_name,
                scope: c.scope.clone(),
                sub_items: Vec::new(),
            }
        })
        .collect();

    rows.sort_by_key(|r| std::cmp::Reverse(r.bytes));
    CategoryDetail {
        total_bytes: total.bytes,
        rows,
    }
}

/// Detailed human information (label, description, icon_name) for one contribution row.
pub fn detail_info(category: Category, path: &Path, home: &Path) -> (String, String, String) {
    let rel = path.strip_prefix(home).unwrap_or(path);
    let rel_str = rel.to_str().unwrap_or("");

    match (category, rel_str) {
        // Applications
        (Category::Applications, ".local/share/flatpak") => (
            "Flatpak applications".to_string(),
            "Installed Flatpak applications".to_string(),
            "apps".to_string(),
        ),
        (Category::Applications, ".var/app") => (
            "Flatpak application data".to_string(),
            "Data belonging to Flatpak applications".to_string(),
            "folder".to_string(),
        ),
        (Category::Applications, ".local/share/Steam") => (
            "Steam games and data".to_string(),
            "Installed games and game data".to_string(),
            "apps".to_string(),
        ),
        (Category::Applications, ".steam") => (
            "Steam library".to_string(),
            "Steam client and runtime files".to_string(),
            "apps".to_string(),
        ),
        (Category::Applications, ".local/share/applications") => (
            "Application launchers".to_string(),
            "Desktop menu shortcuts and launchers".to_string(),
            "apps".to_string(),
        ),
        (Category::Applications, _) => (
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Application data".to_string()),
            "Data created by installed applications".to_string(),
            "apps".to_string(),
        ),

        // Temporary
        (Category::Temporary, ".cache") => (
            "Application cache".to_string(),
            "Cached data from applications and web browsers".to_string(),
            "temp".to_string(),
        ),
        (Category::Temporary, _) => (
            "Temporary files".to_string(),
            "Application and temporary cache data".to_string(),
            "temp".to_string(),
        ),

        // Trash
        (Category::Trash, ".local/share/Trash") | (Category::Trash, _) => (
            "Deleted files".to_string(),
            "Files waiting to be permanently removed".to_string(),
            "trash".to_string(),
        ),

        // Videos
        (Category::Videos, _) => (
            "Videos".to_string(),
            "Video recordings, movies and clips".to_string(),
            "video".to_string(),
        ),

        // Pictures
        (Category::Pictures, _) => (
            "Pictures".to_string(),
            "Photos, screenshots and image libraries".to_string(),
            "image".to_string(),
        ),

        // Music
        (Category::Music, _) => (
            "Music".to_string(),
            "Audio files, music tracks and albums".to_string(),
            "music".to_string(),
        ),

        // Downloads
        (Category::Downloads, _) => (
            "Downloads".to_string(),
            "Files downloaded from web and browsers".to_string(),
            "download".to_string(),
        ),

        // Documents
        (Category::Documents, "Documents") => (
            "Documents".to_string(),
            "Text documents, PDFs and work files".to_string(),
            "document".to_string(),
        ),
        (Category::Documents, _) => (
            path.file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "Documents and files".to_string()),
            "Document files and notes".to_string(),
            "document".to_string(),
        ),

        // Other - Explainable breakdown
        (Category::Other, ".config") => (
            "Application configuration".to_string(),
            "Settings and preferences for desktop applications".to_string(),
            "settings".to_string(),
        ),
        (Category::Other, ".local") => (
            "Local application data".to_string(),
            "User-level application data, state and libraries".to_string(),
            "folder".to_string(),
        ),
        (Category::Other, ".npm") => (
            "Node.js package cache".to_string(),
            "Cached packages and npm module data".to_string(),
            "folder".to_string(),
        ),
        (Category::Other, ".cargo") => (
            "Rust Cargo packages".to_string(),
            "Downloaded crates and build artifacts".to_string(),
            "folder".to_string(),
        ),
        (Category::Other, ".rustup") => (
            "Rust toolchains".to_string(),
            "Installed Rust compilers and documentation".to_string(),
            "folder".to_string(),
        ),
        (Category::Other, ".antigravity-ide")
        | (Category::Other, ".vscode")
        | (Category::Other, ".cursor")
        | (Category::Other, ".zed") => (
            "Developer IDE data".to_string(),
            "Editor workspace cache and extensions".to_string(),
            "folder".to_string(),
        ),
        (Category::Other, ".docker") => (
            "Docker container data".to_string(),
            "Local container images and configuration".to_string(),
            "folder".to_string(),
        ),
        (Category::Other, ".gradle") | (Category::Other, ".m2") => (
            "Build tool cache".to_string(),
            "Downloaded dependencies and build caches".to_string(),
            "folder".to_string(),
        ),
        (Category::Other, ".mozilla")
        | (Category::Other, ".google-chrome")
        | (Category::Other, ".chromium") => (
            "Web browser data".to_string(),
            "Browser settings, profiles and history".to_string(),
            "folder".to_string(),
        ),
        (Category::Other, ".ssh") => (
            "SSH keys and config".to_string(),
            "Secure shell keys and configuration".to_string(),
            "folder".to_string(),
        ),
        (Category::Other, ".gnupg") => (
            "GPG keys and config".to_string(),
            "Encryption keys and credentials".to_string(),
            "folder".to_string(),
        ),
        (Category::System, _) => {
            let name = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "System".to_string());
            let path_str = path.to_str().unwrap_or("");
            let (label, desc) = if path_str == "/usr" || name == "usr" || rel_str == "usr" {
                ("System software", "Core Linux files")
            } else if path_str == "/var" || name == "var" || rel_str == "var" {
                (
                    "System & application data",
                    "Services, databases, logs and state",
                )
            } else if path_str == "/opt" || name == "opt" || rel_str == "opt" {
                (
                    "Optional software",
                    "Software installed outside the main system",
                )
            } else if path_str == "/boot" || name == "boot" || rel_str == "boot" {
                ("Boot files", "Kernels and boot files")
            } else if path_str == "/etc" || name == "etc" || rel_str == "etc" {
                ("Configuration", "System-wide settings")
            } else if path_str == "/root" || name == "root" || rel_str == "root" {
                ("Superuser storage", "Administrator files")
            } else if path_str == "/srv" || name == "srv" || rel_str == "srv" {
                ("Service data", "Data for system and web services")
            } else if path_str.starts_with("/var/lib/libvirt")
                || path_str.contains("docker")
                || path_str.contains("containers")
            {
                (
                    "Virtual machines & containers",
                    "Virtual machine disks, images and container storage",
                )
            } else if path_str.starts_with("/var/lib/flatpak") || path_str.contains("snapd") {
                (
                    "Application state",
                    "Application runtimes and service state",
                )
            } else if path_str.starts_with("/var/lib/pacman")
                || path_str.contains("dpkg")
                || path_str.contains("rpm")
            {
                ("Package data", "Package manager database and metadata")
            } else if path_str.starts_with("/var/cache") {
                ("Package cache", "Package download caches")
            } else if path_str.starts_with("/var/log") {
                ("Logs & journals", "System logs and journal data")
            } else if path_str.starts_with("/var/lib/systemd") {
                ("System services", "Service state and system databases")
            } else {
                ("Other system data", "Remaining system and service files")
            };
            (label.to_string(), desc.to_string(), "settings".to_string())
        }
        (Category::Other, _) => {
            let label = path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| category.display_name().to_string());
            (
                label,
                "Additional user files and storage".to_string(),
                "folder".to_string(),
            )
        }
    }
}

/// Classify an Other contribution into one of the canonical human categories.
pub fn other_group_category(
    path: &Path,
    home: &Path,
    label: &str,
) -> (&'static str, &'static str, &'static str) {
    let rel = path.strip_prefix(home).unwrap_or(path);
    let rel_str = rel.to_string_lossy().to_lowercase();
    let label_lower = label.to_lowercase();

    // 1. Browser data
    if rel_str.contains("mozilla")
        || rel_str.contains("chrome")
        || rel_str.contains("chromium")
        || rel_str.contains("brave")
        || rel_str.contains("edge")
        || rel_str.contains("floorp")
        || rel_str.contains("librewolf")
        || rel_str.contains("waterfox")
        || rel_str.contains("zen")
        || rel_str.contains("opera")
        || rel_str.contains("vivaldi")
        || label_lower.contains("browser")
    {
        return ("Browser data", "Browser profiles and cached data", "folder");
    }

    // 2. Game data
    if rel_str.contains("game")
        || rel_str.contains("wine")
        || rel_str.contains("heroic")
        || rel_str.contains("lutris")
        || rel_str.contains("retroarch")
        || rel_str.contains("emulator")
        || label_lower.contains("game")
    {
        return ("Game data", "Game-related files", "folder");
    }

    // 3. Developer data
    if rel_str.contains("cargo")
        || rel_str.contains("rust")
        || rel_str.contains("npm")
        || rel_str.contains("nvm")
        || rel_str.contains("yarn")
        || rel_str.contains("pnpm")
        || rel_str.contains("vscode")
        || rel_str.contains("cursor")
        || rel_str.contains("antigravity")
        || rel_str.contains("gemini")
        || rel_str.contains("claude")
        || rel_str.contains("codex")
        || rel_str.contains("copilot")
        || rel_str.contains("gsd")
        || rel_str.contains("hermes")
        || rel_str.contains("ollama")
        || rel_str.contains("opencode")
        || rel_str.contains("docker")
        || rel_str.contains("gradle")
        || rel_str.contains(".m2")
        || rel_str.contains("dotnet")
        || rel_str.contains("electron")
        || rel_str.contains("java")
        || rel_str.contains("subversion")
        || rel_str.contains(".git")
        || rel_str.contains("project")
        || rel_str.contains("dev")
        || rel_str.contains("code")
        || rel_str.contains("zed")
        || label_lower.contains("developer")
        || label_lower.contains("rust")
        || label_lower.contains("node")
    {
        return (
            "Developer data",
            "Projects, build files and packages",
            "folder",
        );
    }

    // 4. Cache data (if explicitly in Other)
    if rel_str.contains("cache") || label_lower.contains("cache") {
        return ("Cache data", "User cache and temporary data", "temp");
    }

    // 5. Application data
    if rel_str.starts_with(".local")
        || rel_str.starts_with(".config")
        || rel_str.starts_with(".var")
        || rel_str.starts_with(".gnupg")
        || rel_str.starts_with(".pki")
        || rel_str.starts_with(".ssh")
        || rel_str.contains("spicetify")
        || rel_str.contains("stremio")
        || rel_str.contains("icon")
        || rel_str.contains("theme")
        || label_lower.contains("application")
        || label_lower.contains("configuration")
    {
        return (
            "Application data",
            "Application data and local storage",
            "folder",
        );
    }

    // 6. Unclassified
    (
        "Unclassified",
        "Files not yet assigned to another category",
        "folder",
    )
}

/// Human-readable label for one contribution. Known leaves win; anything
/// else falls back to its plain file name (a name, never a raw path).
#[allow(dead_code)]
pub fn detail_label(category: Category, path: &Path, home: &Path) -> String {
    detail_info(category, path, home).0
}

/// "N files" meta line for a detail row.
pub fn files_meta(files: u64) -> String {
    if files == 1 {
        "1 file".to_string()
    } else {
        format!("{files} files")
    }
}

/// Detail list height for a window of `window_h` logical px: the page
/// minus detail chrome (back 36 + header 52 + section 20 + total 52 +
/// gaps/padding 108 = 268). Floor keeps degenerate windows sane.
pub fn detail_list_height(window_h: f32) -> f32 {
    (window_h - 144.0 - 268.0).max(60.0)
}

/// Scroll canvas for `rows` 64px detail rows (8px gaps, 16px padding).
/// Zero rows collapse the canvas; the UI shows its empty state instead.
pub fn detail_viewport_height(rows: usize) -> f32 {
    if rows == 0 {
        0.0
    } else {
        rows as f32 * 56.0 + rows.saturating_sub(1) as f32 * 8.0 + 16.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::classify::ClassificationRules;
    use crate::scan::{ScanOptions, scan_blocking};
    use crate::xdg::XdgDirs;
    use std::sync::atomic::AtomicBool;

    fn write(home: &Path, rel: &str, bytes: usize) {
        let path = home.join(rel);
        std::fs::create_dir_all(path.parent().expect("test file has parent")).unwrap();
        std::fs::write(path, vec![0u8; bytes]).unwrap();
    }

    fn test_xdg(home: &Path) -> XdgDirs {
        XdgDirs {
            documents: home.join("Documents"),
            downloads: home.join("Downloads"),
            music: home.join("Music"),
            pictures: home.join("Pictures"),
            videos: home.join("Videos"),
        }
    }

    fn classified(home_files: &[(&str, usize)]) -> (tempfile::TempDir, StorageClassification) {
        let dir = tempfile::tempdir().unwrap();
        for (rel, bytes) in home_files {
            write(dir.path(), rel, *bytes);
        }
        let rules = ClassificationRules::with_xdg(dir.path(), &test_xdg(dir.path()));
        let options = ScanOptions {
            root: dir.path().to_path_buf(),
            follow_symlinks: false,
            track: rules.wanted_paths(),
        };
        let scan = scan_blocking(&options, &AtomicBool::new(false), |_| {});
        let classification = rules.classify(&scan);
        assert!(classification.partition_ok());
        (dir, classification)
    }

    #[test]
    fn known_leaves_get_friendly_labels() {
        let home = Path::new("/home/t");
        let cases = [
            (Category::Trash, ".local/share/Trash", "Deleted files"),
            (Category::Temporary, ".cache", "Application cache"),
            (
                Category::Applications,
                ".local/share/flatpak",
                "Flatpak applications",
            ),
            (
                Category::Applications,
                ".var/app",
                "Flatpak application data",
            ),
            (
                Category::Applications,
                ".local/share/Steam",
                "Steam games and data",
            ),
            (Category::Applications, ".steam", "Steam library"),
            (
                Category::Applications,
                ".local/share/applications",
                "Application launchers",
            ),
            (Category::Other, ".config", "Application configuration"),
            (Category::Other, ".local", "Local application data"),
            (Category::Other, ".cargo", "Rust Cargo packages"),
            (Category::Other, ".rustup", "Rust toolchains"),
            (Category::Other, ".npm", "Node.js package cache"),
        ];
        for (category, rel, expected) in cases {
            assert_eq!(detail_label(category, &home.join(rel), home), expected);
        }
    }

    #[test]
    fn unknown_paths_fall_back_to_names() {
        let home = Path::new("/home/t");
        assert_eq!(
            detail_label(Category::Other, &home.join(".unmapped_tool"), home),
            ".unmapped_tool"
        );
        assert_eq!(
            detail_label(Category::Documents, &home.join("Documents/notes.txt"), home),
            "notes.txt"
        );
        // Category-gated: an XDG dir pointing at .cache keeps Documents
        // meaning instead of borrowing the Temporary leaf label.
        assert_eq!(
            detail_label(Category::Documents, &home.join(".cache"), home),
            ".cache"
        );
    }

    #[test]
    fn detail_rows_mirror_contributions_and_sort_descending() {
        let (_dir, c) = classified(&[
            (".local/share/flatpak/a", 300),
            (".local/share/Steam/s", 500),
            (".local/share/applications/d.desktop", 10),
        ]);
        let detail = detail_for(&c, _dir.path(), Category::Applications);
        assert_eq!(detail.total_bytes, 810);
        let labels: Vec<_> = detail.rows.iter().map(|r| r.label.as_str()).collect();
        // Largest first (500 > 300 > 10)
        assert_eq!(
            labels,
            vec![
                "Steam games and data",
                "Flatpak applications",
                "Application launchers"
            ]
        );
        assert_eq!(detail.rows[0].bytes, 500);
        assert_eq!(detail.rows[1].bytes, 300);
        assert_eq!(detail.rows[2].bytes, 10);
        let sum: u64 = detail.rows.iter().map(|r| r.bytes).sum();
        assert_eq!(sum, detail.total_bytes);
    }

    #[test]
    fn zero_byte_category_is_empty() {
        let (_dir, c) = classified(&[("Documents/a.txt", 10)]);
        let detail = detail_for(&c, _dir.path(), Category::Trash);
        assert_eq!(detail.total_bytes, 0);
        assert!(detail.rows.is_empty());
    }

    #[test]
    fn other_detail_explains_remainder() {
        let (_dir, c) = classified(&[
            (".config/a.dat", 500),
            (".local/junk.dat", 100),
            ("Projects/p.dat", 50),
        ]);
        let detail = detail_for(&c, _dir.path(), Category::Other);
        assert_eq!(detail.total_bytes, 650);
        let labels: Vec<_> = detail.rows.iter().map(|r| r.label.as_str()).collect();
        assert!(labels.contains(&"Application data"));
        assert!(labels.contains(&"Developer data"));
        let sub_labels: Vec<_> = detail
            .rows
            .iter()
            .flat_map(|r| r.sub_items.iter().map(|s| s.label.as_str()))
            .collect();
        assert!(sub_labels.contains(&"Application configuration"));
        assert!(sub_labels.contains(&"Local application data"));
        assert!(sub_labels.contains(&"Projects"));
        let sum: u64 = detail.rows.iter().map(|r| r.bytes).sum();
        assert_eq!(sum, 650);
    }

    #[test]
    fn files_meta_singular_plural() {
        assert_eq!(files_meta(0), "0 files");
        assert_eq!(files_meta(1), "1 file");
        assert_eq!(files_meta(42), "42 files");
    }

    #[test]
    fn detail_geometry_formulas() {
        assert_eq!(detail_list_height(760.0), 348.0);
        assert_eq!(detail_list_height(650.0), 238.0);
        assert_eq!(detail_list_height(0.0), 60.0);
        assert_eq!(detail_viewport_height(0), 0.0);
        assert_eq!(detail_viewport_height(1), 72.0);
        assert_eq!(detail_viewport_height(4), 264.0);
    }

    #[test]
    fn single_contribution_detail() {
        let (_dir, c) = classified(&[("Downloads/a.zip", 77)]);
        let detail = detail_for(&c, _dir.path(), Category::Downloads);
        assert_eq!(detail.rows.len(), 1);
        assert_eq!(detail.rows[0].bytes, 77);
        // Owned dir labelled with friendly concept
        assert_eq!(detail.rows[0].label, "Downloads");
        assert_eq!(
            detail.rows[0].description,
            "Files downloaded from web and browsers"
        );
        assert_eq!(detail.rows[0].icon_name, "download");
    }

    #[test]
    fn all_categories_detail_consistent_with_m4_provenance() {
        let (_dir, c) = classified(&[
            (".local/share/flatpak/app", 1000),
            (".local/share/Steam/game", 500),
            ("Videos/clip.mp4", 3000),
            ("Pictures/photo.png", 2000),
            ("Downloads/file.iso", 1500),
            ("Documents/doc.pdf", 800),
            (".cache/browser", 400),
            (".local/share/Trash/del", 700),
            (".config/settings", 250),
        ]);

        for category in Category::ALL {
            let detail = detail_for(&c, _dir.path(), category);
            // Category total exactly matches M4 classification
            assert_eq!(detail.total_bytes, c.of(category).bytes);

            // Sorting invariant: largest contributors strictly descending
            for window in detail.rows.windows(2) {
                assert!(
                    window[0].bytes >= window[1].bytes,
                    "rows must be sorted descending"
                );
            }

            // No double counting: sum of rows does not exceed total_bytes
            let row_sum: u64 = detail.rows.iter().map(|r| r.bytes).sum();
            assert!(row_sum <= detail.total_bytes);

            // Human friendly labels and descriptions: no raw /home/... paths in UI rows
            for row in &detail.rows {
                assert!(!row.label.starts_with("/"));
                assert!(!row.description.is_empty());
                assert!(!row.icon_name.is_empty());
            }
        }
    }

    #[test]
    fn empty_category_produces_zero_rows_and_zero_bytes() {
        let (_dir, c) = classified(&[("Documents/notes.txt", 50)]);
        let detail = detail_for(&c, _dir.path(), Category::Music);
        assert_eq!(detail.total_bytes, 0);
        assert_eq!(detail.rows.len(), 0);
    }
    #[test]
    fn test_other_breakdown_human_categories_and_subitems() {
        let (_dir, c) = classified(&[
            (".cargo/bin/tool", 1000),
            (".vscode/extensions/ext", 500),
            (".mozilla/firefox/profile", 800),
            ("Games/doom/wad", 1200),
            (".local/share/data", 300),
            ("misc.dat", 100),
        ]);
        let detail = detail_for(&c, _dir.path(), Category::Other);
        assert_eq!(detail.total_bytes, 3900);
        let labels: Vec<_> = detail.rows.iter().map(|r| r.label.as_str()).collect();

        // Must display human-first functional groups
        assert!(labels.contains(&"Game data"));
        assert!(labels.contains(&"Developer data"));
        assert!(labels.contains(&"Browser data"));
        assert!(labels.contains(&"Application data"));
        assert!(labels.contains(&"Unclassified"));

        // Exact integer sum invariant
        let sum: u64 = detail.rows.iter().map(|r| r.bytes).sum();
        assert_eq!(sum, 3900);

        // Sub-items must contain the detailed child directories
        let dev_row = detail
            .rows
            .iter()
            .find(|r| r.label == "Developer data")
            .unwrap();
        assert_eq!(dev_row.bytes, 1500);
        let dev_sub_labels: Vec<_> = dev_row.sub_items.iter().map(|s| s.label.as_str()).collect();
        assert!(dev_sub_labels.contains(&"Rust Cargo packages"));
        assert!(dev_sub_labels.contains(&"Developer IDE data"));

        let browser_row = detail
            .rows
            .iter()
            .find(|r| r.label == "Browser data")
            .unwrap();
        assert_eq!(browser_row.bytes, 800);
        assert_eq!(browser_row.sub_items[0].label, "Web browser data");
    }

    #[test]
    fn test_system_detail_with_var_subcontributors() {
        let home = Path::new("/home/user");
        let mut classification = StorageClassification {
            total_bytes: 53_000,
            total_files: 100,
            categories: Category::ALL
                .iter()
                .map(|&cat| crate::classify::CategoryTotal {
                    category: cat,
                    bytes: if cat == Category::System { 53_000 } else { 0 },
                    files: 100,
                    contributions: Vec::new(),
                })
                .collect(),
            error_count: 0,
            notes: Vec::new(),
        };

        // Create System contributions with /var and its sub_contributions
        let var_sub = vec![
            crate::classify::Contribution {
                path: PathBuf::from("/var/lib/libvirt"),
                bytes: 8_000,
                files: 5,
                detail: "Virtual machines & containers".to_string(),
                scope: ContributionScope::whole_subtree(PathBuf::from("/var/lib/libvirt")),
                sub_contributions: Vec::new(),
            },
            crate::classify::Contribution {
                path: PathBuf::from("/var/lib/flatpak"),
                bytes: 4_000,
                files: 20,
                detail: "Application state".to_string(),
                scope: ContributionScope::whole_subtree(PathBuf::from("/var/lib/flatpak")),
                sub_contributions: Vec::new(),
            },
            crate::classify::Contribution {
                path: PathBuf::from("/var"),
                bytes: 2_000,
                files: 15,
                detail: "Other system data".to_string(),
                scope: ContributionScope::whole_subtree(PathBuf::from("/var")),
                sub_contributions: Vec::new(),
            },
        ];

        let sys_total = classification
            .categories
            .iter_mut()
            .find(|c| c.category == Category::System)
            .unwrap();

        sys_total.contributions.push(crate::classify::Contribution {
            path: PathBuf::from("/usr"),
            bytes: 39_000,
            files: 60,
            detail: "Core Linux files".to_string(),
            scope: ContributionScope::whole_subtree(PathBuf::from("/usr")),
            sub_contributions: Vec::new(),
        });

        sys_total.contributions.push(crate::classify::Contribution {
            path: PathBuf::from("/var"),
            bytes: 14_000,
            files: 40,
            detail: "Services, databases, logs and state".to_string(),
            scope: ContributionScope::whole_subtree(PathBuf::from("/var")),
            sub_contributions: var_sub,
        });

        let detail = detail_for(&classification, home, Category::System);
        assert_eq!(detail.total_bytes, 53_000);
        assert_eq!(detail.rows.len(), 2);

        // System software has empty sub_items (goes to explorer directly)
        let usr_row = detail
            .rows
            .iter()
            .find(|r| r.label == "System software")
            .unwrap();
        assert_eq!(usr_row.bytes, 39_000);
        assert!(usr_row.sub_items.is_empty());

        // System & application data has 3 sub_items
        let var_row = detail
            .rows
            .iter()
            .find(|r| r.label == "System & application data")
            .unwrap();
        assert_eq!(var_row.bytes, 14_000);
        assert_eq!(var_row.sub_items.len(), 3);

        // Sum of sub_items equals /var total
        let sub_sum: u64 = var_row.sub_items.iter().map(|s| s.bytes).sum();
        assert_eq!(sub_sum, 14_000);
        assert_eq!(var_row.sub_items[0].label, "Virtual machines & containers");
        assert_eq!(var_row.sub_items[0].bytes, 8_000);
    }
}
