//! File & folder exploration model (Milestone 6).
//!
//! Provides in-place directory navigation for category contributors
//! without rescanning the home directory. All data is queried from
//! the in-memory [`crate::scan::FileTree`] built during the initial scan.
//!
//! Navigation is O(1) via direct `DirectoryId` indexing. No raw absolute
//! filesystem paths are ever exposed in display models.
//! The classifier remains the source of truth for contributor scope.

use crate::classify::ContributionScope;
use crate::scan::FileTree;
use std::path::Path;

/// Page size for directory pagination to keep Slint rendering responsive.
pub const DEFAULT_PAGE_SIZE: usize = 100;

/// Sort mode for directory exploration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortMode {
    #[default]
    SizeDesc,
    SizeAsc,
    NameAsc,
    NameDesc,
    Type,
}

impl SortMode {
    pub fn label(self) -> &'static str {
        match self {
            SortMode::SizeDesc => "Size ↓",
            SortMode::SizeAsc => "Size ↑",
            SortMode::NameAsc => "Name A-Z",
            SortMode::NameDesc => "Name Z-A",
            SortMode::Type => "Type",
        }
    }

    /// Parse a sort-key string sent from the UI sort popover.
    pub fn from_key(key: &str) -> Self {
        match key {
            "size_asc"  => SortMode::SizeAsc,
            "name_asc"  => SortMode::NameAsc,
            "name_desc" => SortMode::NameDesc,
            "type"      => SortMode::Type,
            _           => SortMode::SizeDesc, // "size_desc" or unknown → default
        }
    }

    /// Machine key for this sort mode (used as value in sort popover).
    #[allow(dead_code)]
    pub fn key(self) -> &'static str {
        match self {
            SortMode::SizeDesc => "size_desc",
            SortMode::SizeAsc  => "size_asc",
            SortMode::NameAsc  => "name_asc",
            SortMode::NameDesc => "name_desc",
            SortMode::Type     => "type",
        }
    }
}

/// Display model for a single file or folder row in the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExplorerRowModel {
    pub name: String,
    pub is_dir: bool,
    pub bytes: u64,
    pub meta: String,
    pub file_type: String,
    pub icon_name: String,
    /// Direct directory ID for O(1) child navigation.
    pub dir_id: Option<u32>,
}

/// View model for an active directory exploration view.
#[derive(Debug, Clone)]
pub struct ExplorerPage {
    pub title: String,
    pub total_bytes: u64,
    pub parent_title: String,
    pub breadcrumb: String,
    pub rows: Vec<ExplorerRowModel>,
    #[allow(dead_code)]
    pub total_items: usize,
    pub current_page: usize,
    pub total_pages: usize,
    pub summary: String,
    pub empty: bool,
    pub error: Option<String>,
}

/// Map file names and directory status to standard DiskScout icon keys.
pub fn entry_icon(name: &str, is_dir: bool) -> &'static str {
    if is_dir {
        return "folder";
    }
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        // Video files
        "mp4" | "mkv" | "avi" | "mov" | "webm" | "flv" | "wmv" | "m4v" | "ts" => "video",
        // Image files
        "png" | "jpg" | "jpeg" | "webp" | "svg" | "gif" | "bmp" | "ico" | "tiff" => "image",
        // Audio files
        "mp3" | "flac" | "wav" | "ogg" | "m4a" | "aac" | "opus" | "wma" => "music",
        // Document & generic files
        _ => "document",
    }
}

/// Human-friendly file type display name.
pub fn file_type_display(name: &str, is_dir: bool, file_count: u64) -> String {
    if is_dir {
        if file_count == 1 {
            return "Folder (1 item)".to_string();
        } else if file_count > 1 {
            return format!("Folder ({} items)", format_count(file_count));
        } else {
            return "Folder".to_string();
        }
    }
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();

    match ext.as_str() {
        "mkv" => "MKV Video".to_string(),
        "mp4" => "MP4 Video".to_string(),
        "avi" => "AVI Video".to_string(),
        "webm" => "WebM Video".to_string(),
        "mov" => "QuickTime Video".to_string(),
        "png" => "PNG Image".to_string(),
        "jpg" | "jpeg" => "JPEG Image".to_string(),
        "webp" => "WebP Image".to_string(),
        "svg" => "SVG Image".to_string(),
        "gif" => "GIF Image".to_string(),
        "mp3" => "MP3 Audio".to_string(),
        "flac" => "FLAC Audio".to_string(),
        "wav" => "WAV Audio".to_string(),
        "ogg" => "OGG Audio".to_string(),
        "rs" => "Rust Source".to_string(),
        "toml" => "TOML Config".to_string(),
        "json" => "JSON Data".to_string(),
        "txt" => "Plain Text".to_string(),
        "md" => "Markdown".to_string(),
        "pdf" => "PDF Document".to_string(),
        "tar" | "gz" | "xz" | "zip" | "zst" | "bz2" => "Archive".to_string(),
        "deb" | "rpm" | "flatpak" => "Package".to_string(),
        "iso" => "Disk Image".to_string(),
        other if !other.is_empty() => format!("{} File", other.to_ascii_uppercase()),
        _ => "File".to_string(),
    }
}

/// Format the subtitle meta text for an entry.
pub fn entry_meta(is_dir: bool, file_count: u64) -> String {
    if is_dir {
        if file_count == 1 {
            "Folder • 1 file".to_string()
        } else if file_count > 1 {
            format!("Folder • {} files", format_count(file_count))
        } else {
            "Folder".to_string()
        }
    } else {
        "File".to_string()
    }
}

/// Format numbers with comma separators (e.g. 12,345).
pub fn format_count(count: u64) -> String {
    let s = count.to_string();
    let mut out = String::new();
    let chars: Vec<char> = s.chars().collect();
    for (i, c) in chars.iter().enumerate() {
        if i > 0 && (chars.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(*c);
    }
    out
}

/// Resolve a contributor's filesystem path to its in-memory directory ID.
/// Gracefully handles disappeared or restricted paths.
pub fn resolve_contributor_root(tree: &FileTree, contributor_path: &Path) -> Result<u32, String> {
    if let Some(id) = tree.find_dir(contributor_path) {
        return Ok(id);
    }

    // Path not present in scan tree: diagnose why without crashing.
    if !contributor_path.exists() {
        return Err("This folder is no longer available.".to_string());
    }

    if let Err(err) = std::fs::read_dir(contributor_path) {
        if err.kind() == std::io::ErrorKind::PermissionDenied {
            return Err("Access to this folder is restricted.".to_string());
        }
    }

    Err("This folder is empty or not indexed.".to_string())
}

/// Load an exploration page for a directory node by its direct ID.
/// Strictly enforces `ContributionScope` so physical ancestors reflect
/// only data belonging to this contributor.
pub fn load_dir(
    tree: &FileTree,
    dir_id: u32,
    scope: &ContributionScope,
    sort_mode: SortMode,
    page_idx: usize,
    page_size: usize,
    title: &str,
    parent_title: &str,
    breadcrumb: &str,
) -> ExplorerPage {
    let Some(dir) = tree.get_dir(dir_id) else {
        return ExplorerPage {
            title: title.to_string(),
            total_bytes: 0,
            parent_title: parent_title.to_string(),
            breadcrumb: breadcrumb.to_string(),
            rows: Vec::new(),
            total_items: 0,
            current_page: 0,
            total_pages: 0,
            summary: String::new(),
            empty: false,
            error: Some("This folder is no longer available.".to_string()),
        };
    };

    // Scoped directory total
    let (dir_bytes, _) = scope.node_size(&dir.path, dir.bytes, dir.file_count);

    // Filter children strictly belonging to this contributor's scope,
    // and compute each child's scoped size
    struct ScopedChild {
        name: String,
        is_dir: bool,
        bytes: u64,
        file_count: u64,
        dir_id: Option<u32>,
    }

    let mut visible_children: Vec<ScopedChild> = Vec::new();
    for entry in &dir.children {
        let child_path = dir.path.join(&entry.name);
        if !scope.is_included(&child_path) {
            continue;
        }
        let (bytes, file_count) = scope.node_size(&child_path, entry.bytes, entry.file_count);
        visible_children.push(ScopedChild {
            name: entry.name.clone(),
            is_dir: entry.is_dir,
            bytes,
            file_count,
            dir_id: entry.dir_id,
        });
    }

    // Sort according to sort_mode
    match sort_mode {
        SortMode::SizeDesc => {
            visible_children.sort_by(|a, b| {
                b.bytes
                    .cmp(&a.bytes)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
        }
        SortMode::SizeAsc => {
            visible_children.sort_by(|a, b| {
                a.bytes
                    .cmp(&b.bytes)
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
        }
        SortMode::NameAsc => {
            visible_children.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
        }
        SortMode::NameDesc => {
            visible_children.sort_by(|a, b| b.name.to_lowercase().cmp(&a.name.to_lowercase()));
        }
        SortMode::Type => {
            visible_children.sort_by(|a, b| {
                let type_a = file_type_display(&a.name, a.is_dir, a.file_count);
                let type_b = file_type_display(&b.name, b.is_dir, b.file_count);
                type_a.cmp(&type_b).then_with(|| b.bytes.cmp(&a.bytes))
            });
        }
    }

    let total_items = visible_children.len();
    if total_items == 0 {
        return ExplorerPage {
            title: title.to_string(),
            total_bytes: dir_bytes,
            parent_title: parent_title.to_string(),
            breadcrumb: breadcrumb.to_string(),
            rows: Vec::new(),
            total_items: 0,
            current_page: 0,
            total_pages: 0,
            summary: "0 items".to_string(),
            empty: true,
            error: None,
        };
    }

    let p_size = if page_size == 0 {
        DEFAULT_PAGE_SIZE
    } else {
        page_size
    };
    let total_pages = (total_items + p_size - 1) / p_size;
    let safe_page = page_idx.min(total_pages.saturating_sub(1));
    let start = safe_page * p_size;
    let end = (start + p_size).min(total_items);

    let rows: Vec<ExplorerRowModel> = visible_children[start..end]
        .iter()
        .map(|entry| ExplorerRowModel {
            name: entry.name.clone(),
            is_dir: entry.is_dir,
            bytes: entry.bytes,
            meta: entry_meta(entry.is_dir, entry.file_count),
            file_type: file_type_display(&entry.name, entry.is_dir, entry.file_count),
            icon_name: entry_icon(&entry.name, entry.is_dir).to_string(),
            dir_id: entry.dir_id,
        })
        .collect();

    let summary = if total_items <= p_size {
        format!(
            "{} {}",
            total_items,
            if total_items == 1 { "item" } else { "items" }
        )
    } else {
        format!(
            "Showing {}–{} of {} items",
            start + 1,
            end,
            format_count(total_items as u64)
        )
    };

    ExplorerPage {
        title: title.to_string(),
        total_bytes: dir_bytes,
        parent_title: parent_title.to_string(),
        breadcrumb: breadcrumb.to_string(),
        rows,
        total_items,
        current_page: safe_page,
        total_pages,
        summary,
        empty: false,
        error: None,
    }
}

/// Calculate dynamic viewport height for Slint list scroll across view modes.
pub fn explorer_viewport_height(
    row_count: usize,
    view_mode: &str,
    icon_zoom: &str,
    content_w: f32,
) -> f32 {
    if row_count == 0 {
        return 0.0;
    }
    match view_mode {
        "compact" => {
            let rows_h = row_count as f32 * 34.0;
            let gaps_h = (row_count.saturating_sub(1)) as f32 * 4.0;
            rows_h + gaps_h + 16.0
        }
        "details" => {
            let rows_h = row_count as f32 * 36.0;
            let gaps_h = (row_count.saturating_sub(1)) as f32 * 4.0;
            28.0 + rows_h + gaps_h + 16.0
        }
        "icons" => {
            let (cell_w, cell_h): (f32, f32) = match icon_zoom {
                "small" => (112.0, 104.0),
                "large" => (174.0, 154.0),
                _ => (138.0, 126.0),
            };
            let cols = (content_w / cell_w).floor().max(1.0) as usize;
            let num_rows = (row_count + cols - 1) / cols;
            (num_rows as f32 * cell_h) + 16.0
        }
        _ => {
            let rows_h = row_count as f32 * 34.0;
            rows_h + 16.0
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{DirectoryNode, FileEntry};
    use std::path::PathBuf;

    #[test]
    fn test_entry_icon_mapping() {
        assert_eq!(entry_icon("movie.mp4", false), "video");
        assert_eq!(entry_icon("clip.mkv", false), "video");
        assert_eq!(entry_icon("photo.png", false), "image");
        assert_eq!(entry_icon("icon.svg", false), "image");
        assert_eq!(entry_icon("track.mp3", false), "music");
        assert_eq!(entry_icon("song.flac", false), "music");
        assert_eq!(entry_icon("readme.txt", false), "document");
        assert_eq!(entry_icon("data.json", false), "document");
        assert_eq!(entry_icon("steamapps", true), "folder");
    }

    #[test]
    fn test_file_type_display() {
        assert_eq!(file_type_display("Steam", true, 5), "Folder (5 items)");
        assert_eq!(file_type_display("Downloads", true, 0), "Folder");
        assert_eq!(file_type_display("movie.mkv", false, 1), "MKV Video");
        assert_eq!(file_type_display("photo.png", false, 1), "PNG Image");
        assert_eq!(file_type_display("main.rs", false, 1), "Rust Source");
    }

    #[test]
    fn test_sort_mode_from_key() {
        // Direct selection from popover key strings
        assert_eq!(SortMode::from_key("size_desc"), SortMode::SizeDesc);
        assert_eq!(SortMode::from_key("size_asc"),  SortMode::SizeAsc);
        assert_eq!(SortMode::from_key("name_asc"),  SortMode::NameAsc);
        assert_eq!(SortMode::from_key("name_desc"), SortMode::NameDesc);
        assert_eq!(SortMode::from_key("type"),       SortMode::Type);
        // Unknown key falls back to SizeDesc
        assert_eq!(SortMode::from_key("garbage"),    SortMode::SizeDesc);

        // Labels still correct
        assert_eq!(SortMode::SizeDesc.label(), "Size ↓");
        assert_eq!(SortMode::SizeAsc.label(),  "Size ↑");
        assert_eq!(SortMode::NameAsc.label(),  "Name A-Z");
        assert_eq!(SortMode::NameDesc.label(), "Name Z-A");
        assert_eq!(SortMode::Type.label(),     "Type");
    }

    #[test]
    fn test_load_dir_empty() {
        let mut tree = FileTree::default();
        tree.directories.push(DirectoryNode {
            path: PathBuf::from("/test"),
            name: "test".to_string(),
            bytes: 0,
            file_count: 0,
            parent_dir: None,
            children: Vec::new(),
        });
        tree.path_to_dir.insert(PathBuf::from("/test"), 0);

        let scope = ContributionScope::whole_subtree(PathBuf::from("/test"));
        let page = load_dir(
            &tree,
            0,
            &scope,
            SortMode::SizeDesc,
            0,
            100,
            "test",
            "Parent",
            "Parent › test",
        );
        assert!(page.empty);
        assert_eq!(page.rows.len(), 0);
        assert_eq!(page.error, None);
    }

    #[test]
    fn test_load_dir_pagination_and_sorting() {
        let mut tree = FileTree::default();
        let mut children = Vec::new();
        for i in 0..250 {
            children.push(FileEntry {
                name: format!("file_{i:03}.dat"),
                is_dir: false,
                bytes: (300 - i) as u64 * 1024,
                file_count: 1,
                dir_id: None,
            });
        }

        tree.directories.push(DirectoryNode {
            path: PathBuf::from("/large"),
            name: "large".to_string(),
            bytes: 50_000_000,
            file_count: 250,
            parent_dir: None,
            children,
        });
        tree.path_to_dir.insert(PathBuf::from("/large"), 0);

        let scope = ContributionScope::whole_subtree(PathBuf::from("/large"));

        // Page 0: items 0..100 with SizeDesc
        let p0 = load_dir(
            &tree,
            0,
            &scope,
            SortMode::SizeDesc,
            0,
            100,
            "large",
            "Parent",
            "Parent › large",
        );
        assert_eq!(p0.rows.len(), 100);
        assert_eq!(p0.total_pages, 3);
        assert_eq!(p0.current_page, 0);
        assert_eq!(p0.rows[0].name, "file_000.dat");
        assert_eq!(p0.rows[0].bytes, 300 * 1024);
        assert_eq!(p0.summary, "Showing 1–100 of 250 items");

        // Sort by NameAsc
        let p_name = load_dir(
            &tree,
            0,
            &scope,
            SortMode::NameAsc,
            0,
            10,
            "large",
            "Parent",
            "Parent › large",
        );
        assert_eq!(p_name.rows[0].name, "file_000.dat");
        assert_eq!(p_name.rows[1].name, "file_001.dat");
    }

    #[test]
    fn test_missing_dir_returns_error() {
        let tree = FileTree::default();
        let scope = ContributionScope::whole_subtree(PathBuf::from("/missing"));
        let page = load_dir(
            &tree,
            999,
            &scope,
            SortMode::SizeDesc,
            0,
            100,
            "missing",
            "Parent",
            "Parent › missing",
        );
        assert!(page.error.is_some());
        assert_eq!(page.error.unwrap(), "This folder is no longer available.");
    }
    #[test]
    fn test_contributor_scope_invariant_and_restricted_ancestor() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();

        // Structure:
        // ~/.local/share/Steam/game.bin = 20,300 bytes (Applications)
        // ~/.local/share/Trash/trash.bin = 17,200 bytes (Trash)
        // ~/.local/share/icons/icon.png = 6,500 bytes (Other under share)
        // ~/.local/lib/lib.so = 2,400 bytes (Other under lib)
        // ~/.local/state/app.state = 724 bytes (Other under state)
        // ~/.local/bin/tool = 264 bytes (Other under bin)
        let steam_file = home.join(".local/share/Steam/game.bin");
        let trash_file = home.join(".local/share/Trash/trash.bin");
        let icon_file = home.join(".local/share/icons/icon.png");
        let lib_file = home.join(".local/lib/lib.so");
        let state_file = home.join(".local/state/app.state");
        let bin_file = home.join(".local/bin/tool");

        for p in &[
            &steam_file,
            &trash_file,
            &icon_file,
            &lib_file,
            &state_file,
            &bin_file,
        ] {
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        }
        std::fs::write(&steam_file, vec![0u8; 20300]).unwrap();
        std::fs::write(&trash_file, vec![0u8; 17200]).unwrap();
        std::fs::write(&icon_file, vec![0u8; 6500]).unwrap();
        std::fs::write(&lib_file, vec![0u8; 2400]).unwrap();
        std::fs::write(&state_file, vec![0u8; 724]).unwrap();
        std::fs::write(&bin_file, vec![0u8; 264]).unwrap();

        let rules = crate::classify::ClassificationRules::for_home(home);
        let options = crate::scan::ScanOptions {
            root: home.to_path_buf(),
            follow_symlinks: false,
            track: rules.wanted_paths(),
        };
        let scan = crate::scan::scan_blocking(
            &options,
            &std::sync::atomic::AtomicBool::new(false),
            |_| {},
        );
        let classification = rules.classify(&scan);
        assert!(classification.partition_ok());

        assert_eq!(
            classification
                .of(crate::classify::Category::Applications)
                .bytes,
            20300
        );
        assert_eq!(
            classification.of(crate::classify::Category::Trash).bytes,
            17200
        );
        assert_eq!(
            classification.of(crate::classify::Category::Other).bytes,
            9888
        );

        // Find "Local application data" contribution
        let other = classification.of(crate::classify::Category::Other);
        let local_app_contrib = other
            .contributions
            .iter()
            .find(|c| c.path == home.join(".local"))
            .expect("Local application data exists");
        assert_eq!(local_app_contrib.bytes, 9888);

        // Test Explorer page for Local application data root (~/.local)
        let root_dir_id = resolve_contributor_root(&scan.file_tree, &local_app_contrib.path)
            .expect("resolves root");
        let page_root = load_dir(
            &scan.file_tree,
            root_dir_id,
            &local_app_contrib.scope,
            SortMode::SizeDesc,
            0,
            100,
            "Local application data",
            "Other",
            "Other › Local application data",
        );

        // Invariant: page total matches contributor size exactly (9888 bytes, NOT 47388 bytes)
        assert_eq!(page_root.total_bytes, 9888);
        assert_eq!(page_root.total_bytes, local_app_contrib.bytes);

        // Invariant: sum(children.bytes) == contributor.bytes
        let sum_children: u64 = page_root.rows.iter().map(|r| r.bytes).sum();
        assert_eq!(sum_children, 9888);
        assert_eq!(sum_children, local_app_contrib.bytes);

        // Restricted ancestor: share must be restricted to 6500 bytes
        let share_row = page_root
            .rows
            .iter()
            .find(|r| r.name == "share")
            .expect("share directory exists");
        assert_eq!(share_row.bytes, 6500);

        let lib_row = page_root
            .rows
            .iter()
            .find(|r| r.name == "lib")
            .expect("lib directory exists");
        assert_eq!(lib_row.bytes, 2400);

        let state_row = page_root
            .rows
            .iter()
            .find(|r| r.name == "state")
            .expect("state directory exists");
        assert_eq!(state_row.bytes, 724);

        let bin_row = page_root
            .rows
            .iter()
            .find(|r| r.name == "bin")
            .expect("bin directory exists");
        assert_eq!(bin_row.bytes, 264);

        // Drill down into "share" directory
        let share_dir_id = share_row.dir_id.expect("share is a directory with ID");
        let page_share = load_dir(
            &scan.file_tree,
            share_dir_id,
            &local_app_contrib.scope,
            SortMode::SizeDesc,
            0,
            100,
            "share",
            "Local application data",
            "Other › Local application data › share",
        );

        // Share page total must be restricted to 6500 bytes
        assert_eq!(page_share.total_bytes, 6500);

        // Steam and Trash must NOT be present in share
        assert!(
            page_share
                .rows
                .iter()
                .all(|r| r.name != "Steam" && r.name != "Trash")
        );

        // Only "icons" exists, with 6500 bytes
        assert_eq!(page_share.rows.len(), 1);
        assert_eq!(page_share.rows[0].name, "icons");
        assert_eq!(page_share.rows[0].bytes, 6500);
    }
}
