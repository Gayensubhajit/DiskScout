//! Category detail model (Milestone 5, read-only).
//!
//! Answers *"why is this category this large?"* from M4 provenance.
//! No rescans, no filesystem I/O: everything derives from the in-memory
//! [`StorageClassification`]. Nothing here deletes, moves, or otherwise
//! mutates user data; cleanup stays a future milestone.
//!
//! Human-label policy: well-known locations get friendly names
//! ("Flatpak applications"); everything else falls back to its plain
//! directory/file name (honest, never invented semantics). Raw paths stay
//! in Rust for future advanced views and never reach the normal UI.

use std::path::Path;

use crate::classify::{Category, StorageClassification};

/// One breakdown row: an aggregated contribution, never individual files.
#[derive(Debug, Clone)]
pub struct DetailItem {
    pub label: String,
    pub bytes: u64,
    pub files: u64,
}

/// Detail view model for one category.
#[derive(Debug, Clone)]
pub struct CategoryDetail {
    pub total_bytes: u64,
    pub rows: Vec<DetailItem>,
}

/// Build the detail model. Zero-byte categories yield zero rows; the UI
/// shows its empty state from that. Pure function of classified data.
pub fn detail_for(
    classification: &StorageClassification,
    home: &Path,
    category: Category,
) -> CategoryDetail {
    let total = classification.of(category);
    let rows = if total.bytes == 0 {
        Vec::new()
    } else {
        total
            .contributions
            .iter()
            .map(|c| DetailItem {
                label: detail_label(category, &c.path, home),
                bytes: c.bytes,
                files: c.files,
            })
            .collect()
    };
    CategoryDetail {
        total_bytes: total.bytes,
        rows,
    }
}

/// Human-readable label for one contribution. Known leaves win; anything
/// else falls back to its plain file name (a name, never a raw path).
/// Category-gated so a pathological overlap (e.g. an XDG dir pointing at
/// `.cache`) keeps the owning category's meaning, not the leaf's.
pub fn detail_label(category: Category, path: &Path, home: &Path) -> String {
    let rel = path.strip_prefix(home).unwrap_or(path);
    let rel_str = rel.to_str().unwrap_or("");
    let known = match (category, rel_str) {
        (Category::Trash, ".local/share/Trash") => Some("Deleted files"),
        (Category::Temporary, ".cache") => Some("Application cache"),
        (Category::Applications, ".local/share/flatpak") => Some("Flatpak applications"),
        (Category::Applications, ".var/app") => Some("Flatpak application data"),
        (Category::Applications, ".local/share/Steam") => Some("Steam games and data"),
        (Category::Applications, ".steam") => Some("Steam library"),
        (Category::Applications, ".local/share/applications") => Some("Application launchers"),
        (Category::Other, ".config") => Some("Application configuration"),
        (Category::Other, ".local") => Some("Local application data"),
        _ => None,
    };
    known.map(str::to_string).unwrap_or_else(|| {
        path.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| category.display_name().to_string())
    })
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

/// Scroll canvas for `rows` 56px detail rows (8px gaps, 16px padding).
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
        ];
        for (category, rel, expected) in cases {
            assert_eq!(detail_label(category, &home.join(rel), home), expected);
        }
    }

    #[test]
    fn unknown_paths_fall_back_to_names() {
        let home = Path::new("/home/t");
        assert_eq!(
            detail_label(Category::Other, &home.join(".rustup"), home),
            ".rustup"
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
    fn detail_rows_mirror_contributions() {
        let (_dir, c) = classified(&[
            (".local/share/flatpak/a", 300),
            (".local/share/Steam/s", 100),
            (".local/share/applications/d.desktop", 10),
        ]);
        let detail = detail_for(&c, _dir.path(), Category::Applications);
        assert_eq!(detail.total_bytes, 410);
        let labels: Vec<_> = detail.rows.iter().map(|r| r.label.as_str()).collect();
        assert!(labels.contains(&"Flatpak applications"));
        assert!(labels.contains(&"Steam games and data"));
        assert!(labels.contains(&"Application launchers"));
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
        assert!(labels.contains(&"Application configuration"));
        assert!(labels.contains(&"Local application data"));
        assert!(labels.contains(&"Projects"));
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
        // Owned dir labelled by its folder name, honestly.
        assert_eq!(detail.rows[0].label, "Downloads");
    }
}
