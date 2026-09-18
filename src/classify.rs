//! Human storage classification engine (Milestone 4).
//!
//! Answers *"where is my storage going?"* — never *"what can I delete?"*
//! (cleanup is M6/M7). Consumes one [`crate::scan::ScanResult`] plus the
//! nested measurements requested up front; performs **no filesystem I/O**
//! of its own and never rescans.
//!
//! ## Partition guarantee
//!
//! The result is a partition: every scanned byte lands in exactly one
//! category. Explicit rules claim whole subtrees (a top-level entry or a
//! tracked nested path); `Other` is the computed remainder, never measured
//! separately. Location ownership always beats file-type guessing: a video
//! inside `~/Downloads` is Downloads, cache that happens to end in `.mp4`
//! stays Temporary.
//!
//! Extension tables exist only for loose files directly inside the scan
//! root (e.g. `~/photo.jpg` with no `~/Pictures` owning it). Everything
//! inside an owned directory belongs to that directory's category,
//! whatever its extension.
//!
//! Deterministic: fixed rule order, first claim wins, ties impossible.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use crate::scan::ScanResult;
use crate::xdg::XdgDirs;

/// User-facing storage categories, in canonical UI order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Category {
    Applications,
    Documents,
    Videos,
    Pictures,
    Music,
    Downloads,
    Temporary,
    Trash,
    System,
    Other,
}

impl Category {
    /// All categories in canonical UI order. The result always contains
    /// every entry, including empty ones, so the UI has stable slots.
    pub const ALL: [Category; 10] = [
        Category::Applications,
        Category::Documents,
        Category::Videos,
        Category::Pictures,
        Category::Music,
        Category::Downloads,
        Category::Temporary,
        Category::Trash,
        Category::System,
        Category::Other,
    ];

    pub fn display_name(self) -> &'static str {
        match self {
            Category::Applications => "Applications",
            Category::Documents => "Documents",
            Category::Videos => "Videos",
            Category::Pictures => "Pictures",
            Category::Music => "Music",
            Category::Downloads => "Downloads",
            Category::Temporary => "Temporary files",
            Category::Trash => "Trash",
            Category::System => "System",
            Category::Other => "Other",
        }
    }

    pub fn subtitle(self) -> &'static str {
        match self {
            Category::Applications => "Installed applications and games",
            Category::Documents => "Documents and files",
            Category::Videos => "Videos and movies",
            Category::Pictures => "Photos and images",
            Category::Music => "Music and audio files",
            Category::Downloads => "Downloaded files",
            Category::Temporary => "Cache and temporary data",
            Category::Trash => "Deleted files",
            Category::System => "Linux and system data",
            Category::Other => "Other storage",
        }
    }

    /// Slint icon key resolved to `@image-url` literals in AppWindow.slint.
    /// Dynamic image construction is impossible there, so this table is
    /// the single mapping (covered by `icon_names_cover_all_categories`).
    pub fn icon_name(self) -> &'static str {
        match self {
            Category::Applications => "apps",
            Category::Documents => "document",
            Category::Videos => "video",
            Category::Pictures => "image",
            Category::Music => "music",
            Category::Downloads => "download",
            Category::Temporary => "temp",
            Category::Trash => "trash",
            Category::System => "settings",
            Category::Other => "folder",
        }
    }
}

/// Scoped recursive size for a directory within a specific contribution.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct ScopedNodeSize {
    pub bytes: u64,
    pub files: u64,
}

/// Scope definition for a contribution (Milestone 6).
///
/// Preserves exact scoped node identities and sizes so Detail and Explorer
/// show strictly what belongs to this contributor without overcounting or
/// leaking physical ancestors claimed by other categories.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ContributionScope {
    pub root_path: PathBuf,
    /// Explicitly scoped sizes for directories whose physical contents include
    /// data claimed away by other categories.
    pub scoped_nodes: HashMap<PathBuf, ScopedNodeSize>,
    /// Subtrees that were claimed away by other categories and must not be displayed.
    pub claimed_away: Vec<PathBuf>,
}

impl ContributionScope {
    pub fn whole_subtree(root_path: PathBuf) -> Self {
        Self {
            root_path,
            scoped_nodes: HashMap::new(),
            claimed_away: Vec::new(),
        }
    }

    pub fn is_included(&self, path: &Path) -> bool {
        for excluded in &self.claimed_away {
            if path == excluded || path.starts_with(excluded) {
                return false;
            }
        }
        true
    }

    pub fn node_size(&self, path: &Path, raw_bytes: u64, raw_files: u64) -> (u64, u64) {
        if let Some(scoped) = self.scoped_nodes.get(path) {
            (scoped.bytes, scoped.files)
        } else {
            (raw_bytes, raw_files)
        }
    }
}

/// One aggregated contribution: a claimed subtree, never individual files.
/// Bounded by construction (one per rule hit plus capped Other details).
#[derive(Debug, Clone)]
pub struct Contribution {
    pub path: PathBuf,
    pub bytes: u64,
    pub files: u64,
    pub detail: String,
    pub scope: ContributionScope,
    pub sub_contributions: Vec<Contribution>,
}

/// Total for one category.
#[derive(Debug, Clone)]
pub struct CategoryTotal {
    pub category: Category,
    pub bytes: u64,
    pub files: u64,
    pub contributions: Vec<Contribution>,
}

/// Classified result: a partition of the scanner total.
#[derive(Debug, Clone)]
pub struct StorageClassification {
    pub total_bytes: u64,
    pub total_files: u64,
    pub categories: Vec<CategoryTotal>,
    pub error_count: u64,
    /// Paths of interest that fell outside the scan (e.g. an XDG dir on
    /// another volume). Informational only; never counted twice.
    pub notes: Vec<String>,
}

impl StorageClassification {
    pub fn of(&self, category: Category) -> &CategoryTotal {
        self.categories
            .iter()
            .find(|c| c.category == category)
            .expect("classification always contains all categories")
    }

    pub fn category_sum_bytes(&self) -> u64 {
        self.categories.iter().map(|c| c.bytes).sum()
    }

    /// The partition invariant: explicit claims plus the computed
    /// remainder must reconcile exactly with the scanner total.
    pub fn partition_ok(&self) -> bool {
        self.category_sum_bytes() == self.total_bytes
    }
}

/// A whole-subtree ownership claim. `path` must be absolute and inside the
/// scan root; depth-1 paths consume a top-level entry, deeper ones are
/// served from tracked measurements (see [`ClassificationRules::wanted`]).
#[derive(Clone)]
struct OwnedDir {
    path: PathBuf,
    category: Category,
    detail: &'static str,
}

/// A tracked nested leaf claim (depth >= 2 under the root).
#[derive(Clone)]
struct NestedClaim {
    path: PathBuf,
    category: Category,
    detail: &'static str,
}

/// Deterministic rule set for one home directory: precedence-ordered
/// ownership plus the nested leaves the scan must measure up front.
#[derive(Clone)]
pub struct ClassificationRules {
    home: PathBuf,
    /// Whole-subtree claims in precedence order.
    owned: Vec<OwnedDir>,
    /// Nested-leaf claims in precedence order.
    nested: Vec<NestedClaim>,
    /// Rule references outside the scan root (informational only).
    skipped: Vec<String>,
}

impl ClassificationRules {
    /// Production rules: platform location tables plus resolved XDG dirs.
    pub fn for_home(home: &Path) -> Self {
        Self::with_xdg(home, &XdgDirs::resolve(home))
    }

    /// Rules with explicit XDG dirs (tests use this to stay hermetic even
    /// when the developer machine exports its own `XDG_*_DIR` variables).
    pub fn with_xdg(home: &Path, xdg: &XdgDirs) -> Self {
        let mut owned = Vec::new();
        let mut nested = Vec::new();

        // Owned directories first (whole-subtree claims). A nested XDG dir
        // that lives deeper than depth 1 goes to `nested` instead, where
        // the tracked measurement serves it.
        let mut own = |path: PathBuf, category: Category, detail: &'static str| {
            if path == home {
                return;
            }
            let Ok(rel) = path.strip_prefix(home) else {
                return;
            };
            if rel.components().count() >= 2 {
                nested.push(NestedClaim {
                    path,
                    category,
                    detail,
                });
            } else {
                owned.push(OwnedDir {
                    path,
                    category,
                    detail,
                });
            }
        };

        // Precedence: Downloads, Temporary, Applications, then media and
        // documents. Trash is nested-only and claimed before everything
        // (see `classify`). Location ownership beats file types throughout.
        own(xdg.downloads.clone(), Category::Downloads, "XDG downloads");
        for rel in crate::platform::cache_relpaths() {
            own(home.join(rel), Category::Temporary, "known cache location");
        }
        own(home.join(".steam"), Category::Applications, "Steam library");
        for rel in crate::platform::app_storage_relpaths() {
            own(
                home.join(rel),
                Category::Applications,
                "application storage",
            );
        }
        own(xdg.pictures.clone(), Category::Pictures, "XDG pictures");
        own(xdg.videos.clone(), Category::Videos, "XDG videos");
        own(xdg.music.clone(), Category::Music, "XDG music");
        own(xdg.documents.clone(), Category::Documents, "XDG documents");

        // Nested leaves, Trash first for ownership precedence over
        // anything else under `~/.local`.
        let mut nested_ordered = Vec::new();
        for rel in crate::platform::trash_relpaths() {
            nested_ordered.push(NestedClaim {
                path: home.join(rel),
                category: Category::Trash,
                detail: "freedesktop Trash",
            });
        }
        // Nested application leaves (depth >= 2 by construction of the
        // platform tables, but filter defensively: depth-1 entries are
        // already served as top-level summaries).
        for claim in nested
            .iter()
            .filter(|c| c.category == Category::Applications)
        {
            if path_depth_under(claim.path.clone(), home) >= 2 {
                nested_ordered.push(NestedClaim {
                    path: claim.path.clone(),
                    category: claim.category,
                    detail: claim.detail,
                });
            }
        }
        // Any nested owned dirs (unusual XDG layouts) keep their category
        // but sort after the well-known leaves.
        for claim in nested
            .iter()
            .filter(|c| c.category != Category::Applications)
        {
            nested_ordered.push(NestedClaim {
                path: claim.path.clone(),
                category: claim.category,
                detail: claim.detail,
            });
        }

        // `owned` currently mixes depth-1 entries with the depth>=2 ones
        // already moved above; keep only true top-level entries here.
        owned.retain(|o| path_depth_under(o.path.clone(), home) < 2);

        // Anything the rules reference outside the scan root can never be
        // measured: note it once for debuggability instead of silently
        // dropping it.
        let mut skipped = Vec::new();
        for dir in [
            &xdg.documents,
            &xdg.downloads,
            &xdg.music,
            &xdg.pictures,
            &xdg.videos,
        ] {
            if dir == home || dir.strip_prefix(home).is_err() {
                skipped.push(format!(
                    "{} is outside the scan root and stays unscanned",
                    dir.display()
                ));
            }
        }

        Self {
            home: home.to_path_buf(),
            owned,
            nested: nested_ordered,
            skipped,
        }
    }

    /// Rules with fully explicit tables (audit tests only). Allows
    /// constructing overlapping or pathological rule sets that `with_xdg`
    /// can never produce, to prove the engine degrades safely.
    #[cfg(test)]
    pub(crate) fn with_parts(
        home: &Path,
        owned: Vec<(PathBuf, Category, &'static str)>,
        nested: Vec<(PathBuf, Category, &'static str)>,
    ) -> Self {
        Self {
            home: home.to_path_buf(),
            owned: owned
                .into_iter()
                .map(|(path, category, detail)| OwnedDir {
                    path,
                    category,
                    detail,
                })
                .collect(),
            nested: nested
                .into_iter()
                .map(|(path, category, detail)| NestedClaim {
                    path,
                    category,
                    detail,
                })
                .collect(),
            skipped: Vec::new(),
        }
    }

    /// Absolute nested paths the scan must measure (`ScanOptions::track`).
    /// Depth-1 entries need no tracking: the scanner reports them anyway.
    pub fn wanted_paths(&self) -> Vec<PathBuf> {
        self.nested.iter().map(|c| c.path.clone()).collect()
    }

    /// Scan root these rules were built for (detail labels resolve
    /// contributions relative to it).
    pub fn home(&self) -> &Path {
        &self.home
    }

    /// Classify one scan result. Pure function of the result plus these
    /// rules: no filesystem I/O, no rescans.
    pub fn classify(&self, scan: &ScanResult) -> StorageClassification {
        // Index top-level entries by file name for O(1) ownership lookup.
        // (Two depth-1 entries cannot share a file name.)
        let mut top_index: HashMap<&OsStr, usize> = HashMap::new();
        for (i, entry) in scan.top_entries.iter().enumerate() {
            if let Some(name) = entry.path.file_name() {
                top_index.entry(name).or_insert(i);
            }
        }
        let tracked: HashMap<&Path, _> = scan
            .tracked
            .iter()
            .filter(|t| t.found)
            .map(|t| (t.path.as_path(), t))
            .collect();

        // Working state per top-level entry: unclaimed remainder plus a
        // consumed flag. Every byte starts unclaimed; claims move bytes
        // into categories; whatever remains becomes Other.
        struct Remainder {
            bytes: u64,
            files: u64,
        }
        let mut remaining: Vec<Remainder> = scan
            .top_entries
            .iter()
            .map(|e| Remainder {
                bytes: e.bytes,
                files: e.files,
            })
            .collect();
        let mut consumed = vec![false; scan.top_entries.len()];

        let mut totals: HashMap<Category, (u64, u64)> = HashMap::new();
        let mut contributions: HashMap<Category, Vec<Contribution>> = HashMap::new();
        let mut notes = self.skipped.clone();
        for category in Category::ALL {
            totals.insert(category, (0, 0));
            contributions.insert(category, Vec::new());
        }
        #[derive(Debug, Clone)]
        struct ConsumedClaim {
            path: PathBuf,
            bytes: u64,
            files: u64,
        }

        let mut consumed_claims: Vec<ConsumedClaim> = Vec::new();

        fn make_scope(
            root_path: &Path,
            root_bytes: u64,
            root_files: u64,
            claimed_pool: &[ConsumedClaim],
            file_tree: &crate::scan::FileTree,
        ) -> ContributionScope {
            let mut claimed_away = Vec::new();
            let mut relevant_claims = Vec::new();
            for c in claimed_pool {
                if c.path.starts_with(root_path) && c.path != root_path {
                    claimed_away.push(c.path.clone());
                    relevant_claims.push(c);
                }
            }

            let mut scoped_nodes: HashMap<PathBuf, ScopedNodeSize> = HashMap::new();
            if !relevant_claims.is_empty() {
                for claim in &relevant_claims {
                    let mut curr = claim.path.as_path();
                    while let Some(parent) = curr.parent() {
                        if !parent.starts_with(root_path) {
                            break;
                        }
                        let entry = scoped_nodes.entry(parent.to_path_buf()).or_insert_with(|| {
                            if let Some(id) = file_tree.find_dir(parent) {
                                if let Some(node) = file_tree.get_dir(id) {
                                    return ScopedNodeSize {
                                        bytes: node.bytes,
                                        files: node.file_count,
                                    };
                                }
                            }
                            ScopedNodeSize { bytes: 0, files: 0 }
                        });
                        entry.bytes = entry.bytes.saturating_sub(claim.bytes);
                        entry.files = entry.files.saturating_sub(claim.files);

                        if parent == root_path {
                            break;
                        }
                        curr = parent;
                    }
                }
                scoped_nodes.insert(
                    root_path.to_path_buf(),
                    ScopedNodeSize {
                        bytes: root_bytes,
                        files: root_files,
                    },
                );
            }

            ContributionScope {
                root_path: root_path.to_path_buf(),
                scoped_nodes,
                claimed_away,
            }
        }

        /// Add one claimed subtree to a category total with provenance.
        fn add_claim(
            totals: &mut HashMap<Category, (u64, u64)>,
            contributions: &mut HashMap<Category, Vec<Contribution>>,
            category: Category,
            path: PathBuf,
            bytes: u64,
            files: u64,
            detail: &str,
            scope: ContributionScope,
        ) {
            let entry = totals.get_mut(&category).expect("all categories preset");
            entry.0 += bytes;
            entry.1 += files;
            contributions
                .get_mut(&category)
                .expect("all categories preset")
                .push(Contribution {
                    path,
                    bytes,
                    files,
                    detail: detail.to_string(),
                    scope,
                    sub_contributions: Vec::new(),
                });
        }

        // Depth-1 top-level index of an absolute in-root path, if both the
        // path and its top-level ancestor exist in this scan.
        let top_of = |path: &Path| -> Option<usize> {
            let rel = path.strip_prefix(&self.home).ok()?;
            let first = rel.components().next()?;
            let name = first.as_os_str();
            top_index.get(name).copied()
        };

        // 1. Nested leaves in precedence order (Trash first). Each leaf is
        //    subtracted from its top-level ancestor's remainder, so the
        //    ancestor can never report those bytes again. A leaf that
        //    overlaps (ancestor or descendant of) an already-consumed leaf
        //    is skipped with a note: overlapping rules would otherwise
        //    count shared bytes twice. All comparisons are component-wise
        //    (`Path::starts_with`), never string prefixes, so
        //    `Downloads-old` can never match `Downloads`.
        let mut consumed_nested: Vec<PathBuf> = Vec::new();
        for claim in &self.nested {
            let Some(top) = top_of(&claim.path) else {
                continue;
            };
            if consumed[top] {
                continue;
            }
            if consumed_nested
                .iter()
                .any(|done| done.starts_with(&claim.path) || claim.path.starts_with(done))
            {
                notes.push(format!(
                    "overlapping rule skipped: {} (already claimed)",
                    claim.path.display()
                ));
                continue;
            }
            let Some(measured) = tracked.get(claim.path.as_path()) else {
                continue;
            };
            let scope = make_scope(
                &claim.path,
                measured.bytes,
                measured.files,
                &consumed_claims,
                &scan.file_tree,
            );
            add_claim(
                &mut totals,
                &mut contributions,
                claim.category,
                claim.path.clone(),
                measured.bytes,
                measured.files,
                claim.detail,
                scope,
            );
            let rest = &mut remaining[top];
            rest.bytes = rest.bytes.saturating_sub(measured.bytes);
            rest.files = rest.files.saturating_sub(measured.files);
            consumed_nested.push(claim.path.clone());
            consumed_claims.push(ConsumedClaim {
                path: claim.path.clone(),
                bytes: measured.bytes,
                files: measured.files,
            });
            debug_assert!(
                measured.bytes <= scan.top_entries[top].bytes,
                "tracked leaf exceeds its top-level ancestor"
            );
        }

        // 2. Whole-subtree ownership for depth-1 entries, in precedence
        //    order. Takes the CURRENT remainder (nested leaves already
        //    removed), so overlapping rules can never double count.
        for owned in &self.owned {
            let Some(name) = owned.path.file_name() else {
                continue;
            };
            let Some(&top) = top_index.get(name) else {
                continue;
            };
            if consumed[top] {
                continue;
            }
            // Defensive: only claim entries actually below the scan root.
            if scan.top_entries[top].path != owned.path {
                continue;
            }
            let rest = &remaining[top];
            let scope = make_scope(
                &owned.path,
                rest.bytes,
                rest.files,
                &consumed_claims,
                &scan.file_tree,
            );
            add_claim(
                &mut totals,
                &mut contributions,
                owned.category,
                owned.path.clone(),
                rest.bytes,
                rest.files,
                owned.detail,
                scope,
            );
            consumed[top] = true;
        }

        // 3. Loose depth-1 files by extension. Directories are never
        //    extension-classified: unowned ones fall into Other whole.
        for (i, entry) in scan.top_entries.iter().enumerate() {
            if consumed[i] || entry.kind != crate::scan::EntryKind::File {
                continue;
            }
            let Some(category) = extension_category(entry.path.file_name()) else {
                continue;
            };
            let rest = &remaining[i];
            let scope = ContributionScope::whole_subtree(entry.path.clone());
            add_claim(
                &mut totals,
                &mut contributions,
                category,
                entry.path.clone(),
                rest.bytes,
                rest.files,
                "loose file by extension",
                scope,
            );
            consumed[i] = true;
        }

        // 4. Remainder: everything unclaimed becomes Other. This is exactly
        //    `total - explicit`, computed constructively per entry.
        // First, check for high-confidence browser, developer, and game data inside ~/.config
        let config_path = self.home.join(".config");
        let mut nested_other_claims: Vec<(PathBuf, u64, u64, String)> = Vec::new();
        if let Some(top_config) = top_of(&config_path) {
            if !consumed[top_config] {
                let config_subtargets = [
                    (".config/google-chrome", "Google Chrome profile"),
                    (".config/chromium", "Chromium browser profile"),
                    (".config/BraveSoftware", "Brave browser profile"),
                    (".config/microsoft-edge", "Microsoft Edge profile"),
                    (".config/Code", "VS Code developer data"),
                    (".config/Cursor", "Cursor developer data"),
                    (".config/Antigravity IDE", "Antigravity IDE data"),
                    (".config/heroic", "Heroic game launcher data"),
                ];
                for (rel, desc) in config_subtargets {
                    let sub = self.home.join(rel);
                    if let Some(dir_id) = scan.file_tree.find_dir(&sub) {
                        if let Some(node) = scan.file_tree.get_dir(dir_id) {
                            if node.bytes > 0 {
                                nested_other_claims.push((
                                    sub.clone(),
                                    node.bytes,
                                    node.file_count,
                                    desc.to_string(),
                                ));
                                let rest = &mut remaining[top_config];
                                rest.bytes = rest.bytes.saturating_sub(node.bytes);
                                rest.files = rest.files.saturating_sub(node.file_count);
                                consumed_claims.push(ConsumedClaim {
                                    path: sub,
                                    bytes: node.bytes,
                                    files: node.file_count,
                                });
                            }
                        }
                    }
                }
            }
        }

        let mut other_details: Vec<(PathBuf, u64, u64, String)> = Vec::new();
        // Add the nested claims inside ~/.config
        for (p, b, f, desc) in nested_other_claims {
            let (bytes, files) = totals.get_mut(&Category::Other).expect("preset");
            *bytes += b;
            *files += f;
            other_details.push((p, b, f, desc));
        }

        // Add the remaining top-level entries
        for (i, entry) in scan.top_entries.iter().enumerate() {
            if consumed[i] {
                continue;
            }
            let rest = &remaining[i];
            if rest.bytes > 0 || rest.files > 0 {
                other_details.push((
                    entry.path.clone(),
                    rest.bytes,
                    rest.files,
                    "unclaimed remainder".to_string(),
                ));
            }
            let (bytes, files) = totals.get_mut(&Category::Other).expect("preset");
            *bytes += rest.bytes;
            *files += rest.files;
        }

        // Explainable Other: keep the largest unclaimed remainders bounded and ranked.
        other_details.sort_by_key(|a| std::cmp::Reverse(a.1));
        for (path, bytes, files, detail) in other_details.into_iter().take(12) {
            let scope = make_scope(&path, bytes, files, &consumed_claims, &scan.file_tree);
            contributions
                .get_mut(&Category::Other)
                .expect("preset")
                .push(Contribution {
                    path,
                    bytes,
                    files,
                    detail,
                    scope,
                    sub_contributions: Vec::new(),
                });
        }

        let categories = Category::ALL
            .iter()
            .map(|&category| {
                let (bytes, files) = totals[&category];
                CategoryTotal {
                    category,
                    bytes,
                    files,
                    contributions: contributions.remove(&category).unwrap_or_default(),
                }
            })
            .collect();

        StorageClassification {
            total_bytes: scan.total_bytes,
            total_files: scan.file_count,
            categories,
            error_count: scan.error_count,
            notes,
        }
    }
}

/// Depth of `path` below `home` in components, or `usize::MAX` when
/// outside the home tree.
fn path_depth_under(path: PathBuf, home: &Path) -> usize {
    match path.strip_prefix(home) {
        Ok(rel) => rel.components().count(),
        Err(_) => usize::MAX,
    }
}

/// Extension-based category for a loose file name. Only the four media +
/// documents families; everything else (`None`) stays wherever location
/// ownership puts it (usually Other). Lowercase match on the final
/// extension; extensionless names never classify.
fn extension_category(name: Option<&OsStr>) -> Option<Category> {
    let ext = Path::new(name?).extension()?.to_str()?.to_lowercase();
    Some(match ext.as_str() {
        "jpg" | "jpeg" | "png" | "webp" | "gif" | "bmp" | "tif" | "tiff" | "svg" | "ico"
        | "heic" | "heif" | "avif" => Category::Pictures,
        "mp4" | "mkv" | "webm" | "avi" | "mov" | "m4v" | "mpg" | "mpeg" | "wmv" | "flv" | "ogv"
        | "ts" | "3gp" => Category::Videos,
        "mp3" | "flac" | "wav" | "ogg" | "oga" | "opus" | "m4a" | "aac" | "wma" | "aiff" => {
            Category::Music
        }
        "pdf" | "doc" | "docx" | "odt" | "txt" | "rtf" | "md" | "epub" | "mobi" | "azw"
        | "djvu" | "xls" | "xlsx" | "ods" | "ppt" | "pptx" | "odp" | "csv" | "tsv" => {
            Category::Documents
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scan::{ScanOptions, scan_blocking};
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

    /// Build a synthetic home, scan it with the rules' tracked paths, and
    /// classify. Every test asserts the partition invariant through this.
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
        assert!(
            classification.partition_ok(),
            "partition must reconcile: sum={} total={}",
            classification.category_sum_bytes(),
            classification.total_bytes
        );
        (dir, classification)
    }

    fn bytes_of(c: &StorageClassification, category: Category) -> u64 {
        c.of(category).bytes
    }

    #[test]
    fn empty_home_classifies_nothing() {
        let (_dir, c) = classified(&[]);
        assert_eq!(c.total_bytes, 0);
        for category in Category::ALL {
            assert_eq!(bytes_of(&c, category), 0, "{category:?} must be empty");
            assert!(c.of(category).contributions.is_empty());
        }
    }

    #[test]
    fn documents_subtree_by_ownership() {
        // random.bin has no known extension but lives under Documents.
        let (_dir, c) = classified(&[
            ("Documents/a.pdf", 100),
            ("Documents/sub/b.txt", 200),
            ("Documents/random.bin", 50),
        ]);
        assert_eq!(bytes_of(&c, Category::Documents), 350);
        assert_eq!(bytes_of(&c, Category::Other), 0);
    }

    #[test]
    fn media_subtrees_by_location() {
        let (_dir, c) = classified(&[
            ("Pictures/PHOTO.JPG", 100),
            ("Videos/clip.MKV", 200),
            ("Music/song.Flac", 300),
        ]);
        assert_eq!(bytes_of(&c, Category::Pictures), 100);
        assert_eq!(bytes_of(&c, Category::Videos), 200);
        assert_eq!(bytes_of(&c, Category::Music), 300);
    }

    #[test]
    fn downloads_owns_mixed_types() {
        // Location ownership beats file-type guessing: nothing here may
        // leak into Videos/Music/Documents.
        let (_dir, c) = classified(&[
            ("Downloads/movie.mkv", 100),
            ("Downloads/song.mp3", 200),
            ("Downloads/doc.pdf", 300),
        ]);
        assert_eq!(bytes_of(&c, Category::Downloads), 600);
        assert_eq!(bytes_of(&c, Category::Videos), 0);
        assert_eq!(bytes_of(&c, Category::Music), 0);
        assert_eq!(bytes_of(&c, Category::Documents), 0);
    }

    #[test]
    fn cache_owns_its_subtree() {
        // A video inside .cache is Temporary, not Videos.
        let (_dir, c) = classified(&[(".cache/something.mp4", 100), (".cache/a/b.dat", 50)]);
        assert_eq!(bytes_of(&c, Category::Temporary), 150);
        assert_eq!(bytes_of(&c, Category::Videos), 0);
    }

    #[test]
    fn trash_beats_local_remainder() {
        let (_dir, c) = classified(&[
            (".local/share/Trash/files/x", 100),
            (".local/other.dat", 50),
        ]);
        assert_eq!(bytes_of(&c, Category::Trash), 100);
        assert_eq!(bytes_of(&c, Category::Other), 50);
        assert_eq!(bytes_of(&c, Category::Applications), 0);
        let trash = c.of(Category::Trash);
        assert_eq!(trash.contributions.len(), 1);
        assert!(trash.contributions[0].path.ends_with(".local/share/Trash"));
    }

    #[test]
    fn flatpak_and_launchers_are_applications() {
        let (_dir, c) = classified(&[
            (".local/share/flatpak/app/x", 1000),
            (".local/share/applications/app.desktop", 10),
            (".local/junk.dat", 5),
        ]);
        assert_eq!(bytes_of(&c, Category::Applications), 1010);
        // The unclaimed .local remainder falls through to Other exactly.
        assert_eq!(bytes_of(&c, Category::Other), 5);
    }

    #[test]
    fn steam_directory_is_applications() {
        let (_dir, c) = classified(&[(".steam/steamapps/game.dat", 500)]);
        assert_eq!(bytes_of(&c, Category::Applications), 500);
        assert_eq!(bytes_of(&c, Category::Other), 0);
    }

    #[cfg(unix)]
    #[test]
    fn symlinked_library_counts_once() {
        use std::os::unix::fs::symlink;
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Games/real.dat", 500);
        symlink("Games", dir.path().join(".steam")).unwrap();

        let rules = ClassificationRules::with_xdg(dir.path(), &test_xdg(dir.path()));
        let options = ScanOptions {
            root: dir.path().to_path_buf(),
            follow_symlinks: false,
            track: rules.wanted_paths(),
        };
        let scan = scan_blocking(&options, &AtomicBool::new(false), |_| {});
        let c = rules.classify(&scan);

        assert!(c.partition_ok());
        // The link itself is 0 bytes and unowned; the real tree is Other.
        assert_eq!(bytes_of(&c, Category::Applications), 0);
        assert_eq!(bytes_of(&c, Category::Other), 500);
    }

    #[test]
    fn unknown_and_hidden_dirs_are_other() {
        let (_dir, c) = classified(&[
            ("Projects/code.rs", 200),
            ("weird.xyz", 100),
            (".mozilla/x.dat", 100),
            (".rustup/y.dat", 200),
        ]);
        assert_eq!(bytes_of(&c, Category::Other), 600);
    }

    #[test]
    fn loose_files_classify_by_extension() {
        let (_dir, c) = classified(&[
            ("photo.JPG", 100),
            ("notes", 40),
            ("archive.tar.gz", 60),
            ("movie.mkv", 200),
        ]);
        assert_eq!(bytes_of(&c, Category::Pictures), 100);
        assert_eq!(bytes_of(&c, Category::Videos), 200);
        // Extensionless and unknown stay in Other.
        assert_eq!(bytes_of(&c, Category::Other), 100);
    }

    #[test]
    fn nested_files_aggregate() {
        let (_dir, c) = classified(&[("Documents/a/b/c.pdf", 120)]);
        assert_eq!(bytes_of(&c, Category::Documents), 120);
    }

    #[test]
    fn all_categories_present_even_when_empty() {
        let (_dir, c) = classified(&[("Documents/x.txt", 10)]);
        assert_eq!(c.categories.len(), Category::ALL.len());
        for category in Category::ALL {
            if category != Category::Documents {
                assert_eq!(bytes_of(&c, category), 0);
            }
        }
    }

    #[test]
    fn other_is_exact_remainder() {
        let (_dir, c) = classified(&[
            ("Documents/a.txt", 100),
            (".cache/c.dat", 50),
            ("Unowned/d.dat", 25),
        ]);
        let explicit: u64 = Category::ALL
            .iter()
            .filter(|&&cat| cat != Category::Other)
            .map(|&cat| bytes_of(&c, cat))
            .sum();
        assert_eq!(explicit, 150);
        assert_eq!(bytes_of(&c, Category::Other), c.total_bytes - explicit);
        assert_eq!(bytes_of(&c, Category::Other), 25);
    }

    #[test]
    fn other_provenance_is_bounded_and_ranked() {
        let mut files = Vec::new();
        for i in 0..15 {
            files.push((format!("dir{i:02}/f.dat"), 10));
        }
        let owned: Vec<(String, usize)> = files.iter().map(|(rel, _)| (rel.clone(), 10)).collect();
        let refs: Vec<(&str, usize)> = owned.iter().map(|(r, b)| (r.as_str(), *b)).collect();
        let (_dir, c) = classified(&refs);
        assert_eq!(bytes_of(&c, Category::Other), 150);
        let details = &c.of(Category::Other).contributions;
        assert!(details.len() <= 12, "provenance must stay bounded");
        assert!(!details.is_empty());
    }

    #[test]
    fn custom_xdg_locations_win() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "MyDocs/file.txt", 100);
        let xdg = XdgDirs {
            documents: dir.path().join("MyDocs"),
            downloads: dir.path().join("Downloads"),
            music: dir.path().join("Music"),
            pictures: dir.path().join("Pictures"),
            videos: dir.path().join("Videos"),
        };
        let rules = ClassificationRules::with_xdg(dir.path(), &xdg);
        let options = ScanOptions {
            root: dir.path().to_path_buf(),
            follow_symlinks: false,
            track: rules.wanted_paths(),
        };
        let scan = scan_blocking(&options, &AtomicBool::new(false), |_| {});
        let c = rules.classify(&scan);
        assert!(c.partition_ok());
        assert_eq!(bytes_of(&c, Category::Documents), 100);
    }

    #[test]
    fn outside_root_xdg_is_noted_not_counted() {
        let dir = tempfile::tempdir().unwrap();
        write(dir.path(), "Documents/a.txt", 100);
        let xdg = XdgDirs {
            downloads: PathBuf::from("/mnt/external/Downloads"),
            ..test_xdg(dir.path())
        };
        let rules = ClassificationRules::with_xdg(dir.path(), &xdg);
        assert!(
            rules
                .skipped
                .iter()
                .any(|n| n.contains("/mnt/external/Downloads")),
            "outside-root reference must be noted"
        );
        let options = ScanOptions {
            root: dir.path().to_path_buf(),
            follow_symlinks: false,
            track: rules.wanted_paths(),
        };
        let scan = scan_blocking(&options, &AtomicBool::new(false), |_| {});
        let c = rules.classify(&scan);
        assert!(c.partition_ok());
        assert_eq!(bytes_of(&c, Category::Downloads), 0);
        assert!(
            c.notes
                .iter()
                .any(|n| n.contains("/mnt/external/Downloads"))
        );
    }

    #[test]
    fn icon_names_cover_all_categories() {
        let mut names = std::collections::HashSet::new();
        for category in Category::ALL {
            assert!(!category.icon_name().is_empty());
            assert!(!category.display_name().is_empty());
            assert!(!category.subtitle().is_empty());
            names.insert(category.icon_name());
        }
        // Must match the icon-name chain in AppWindow.slint exactly.
        for expected in [
            "apps", "document", "video", "image", "music", "download", "temp", "trash", "settings",
            "folder",
        ] {
            assert!(names.contains(expected), "missing icon key {expected}");
        }
    }

    #[test]
    fn wanted_paths_are_nested_only() {
        let rules = ClassificationRules::with_xdg(
            Path::new("/home/t"),
            &XdgDirs {
                documents: PathBuf::from("/home/t/Documents"),
                downloads: PathBuf::from("/home/t/Downloads"),
                music: PathBuf::from("/home/t/Music"),
                pictures: PathBuf::from("/home/t/Pictures"),
                videos: PathBuf::from("/home/t/Videos"),
            },
        );
        // Depth-1 entries ride along as top-level summaries; only deeper
        // leaves need tracked measurements.
        for path in rules.wanted_paths() {
            let depth = path.strip_prefix("/home/t").unwrap().components().count();
            assert!(depth >= 2, "shallow track is wasteful: {}", path.display());
        }
        assert!(
            rules
                .wanted_paths()
                .iter()
                .any(|p| p.ends_with(".local/share/Trash"))
        );
    }
}

#[cfg(test)]
mod audit_tests {
    use super::*;
    use crate::scan::{ScanOptions, scan_blocking};
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

    /// Scan with caller-supplied rules and assert the partition invariant.
    fn classified_custom(
        home_files: &[(&str, usize)],
        build: impl FnOnce(&Path) -> ClassificationRules,
    ) -> (tempfile::TempDir, StorageClassification) {
        let dir = tempfile::tempdir().unwrap();
        for (rel, bytes) in home_files {
            write(dir.path(), rel, *bytes);
        }
        let rules = build(dir.path());
        let options = ScanOptions {
            root: dir.path().to_path_buf(),
            follow_symlinks: false,
            track: rules.wanted_paths(),
        };
        let scan = scan_blocking(&options, &AtomicBool::new(false), |_| {});
        let classification = rules.classify(&scan);
        assert!(
            classification.partition_ok(),
            "partition must reconcile: sum={} total={}",
            classification.category_sum_bytes(),
            classification.total_bytes
        );
        (dir, classification)
    }

    fn bytes_of(c: &StorageClassification, category: Category) -> u64 {
        c.of(category).bytes
    }

    /// 1. Nested claim inside a wholesale-claimed parent: the nested leaf
    ///    is served first from tracked data, the parent takes the rest.
    #[test]
    fn nested_inside_claimed_parent() {
        let (_dir, c) = classified_custom(
            &[("Downloads/sub/app.dat", 100), ("Downloads/other.dat", 50)],
            |home| {
                ClassificationRules::with_parts(
                    home,
                    vec![(home.join("Downloads"), Category::Downloads, "test owned")],
                    vec![(
                        home.join("Downloads/sub"),
                        Category::Applications,
                        "test nested",
                    )],
                )
            },
        );
        assert_eq!(bytes_of(&c, Category::Applications), 100);
        assert_eq!(bytes_of(&c, Category::Downloads), 50);
        assert_eq!(bytes_of(&c, Category::Other), 0);
    }

    /// 2. Sibling nested leaves under one ancestor: each exact, remainder
    ///    flows to Other without cross-contamination.
    #[test]
    fn sibling_nested_claims() {
        let (_dir, c) = classified_custom(
            &[
                (".local/share/Trash/f", 100),
                (".local/share/flatpak/a", 200),
                (".local/share/Steam/s", 300),
                (".local/share/random.dat", 400),
                (".local/top.dat", 50),
            ],
            |home| ClassificationRules::with_xdg(home, &test_xdg(home)),
        );
        assert_eq!(bytes_of(&c, Category::Trash), 100);
        assert_eq!(bytes_of(&c, Category::Applications), 500);
        assert_eq!(bytes_of(&c, Category::Other), 450);
        assert_eq!(c.total_bytes, 1050);
    }

    /// 3. Two XDG keys pointing at the same directory: first claim
    ///    (Videos precedes Documents) wins, no double count.
    #[test]
    fn overlapping_xdg_paths() {
        let (_dir, c) = classified_custom(&[("Media/a.mp4", 100)], |home| {
            let xdg = XdgDirs {
                documents: home.join("Media"),
                videos: home.join("Media"),
                ..test_xdg(home)
            };
            ClassificationRules::with_xdg(home, &xdg)
        });
        assert_eq!(bytes_of(&c, Category::Videos), 100);
        assert_eq!(bytes_of(&c, Category::Documents), 0);
    }

    /// 4. Downloads absorbs every file type, including unknown ones.
    #[test]
    fn downloads_absorbs_all_types() {
        let (_dir, c) = classified_custom(
            &[
                ("Downloads/movie.mkv", 100),
                ("Downloads/song.flac", 100),
                ("Downloads/doc.pdf", 100),
                ("Downloads/pic.png", 100),
                ("Downloads/random.xyz", 100),
                ("Downloads/noext", 100),
            ],
            |home| ClassificationRules::with_xdg(home, &test_xdg(home)),
        );
        assert_eq!(bytes_of(&c, Category::Downloads), 600);
        for cat in [
            Category::Videos,
            Category::Music,
            Category::Documents,
            Category::Pictures,
        ] {
            assert_eq!(bytes_of(&c, cat), 0, "{cat:?} must not leak from Downloads");
        }
    }

    /// 5. Cache absorbs media/document files by location ownership.
    #[test]
    fn cache_absorbs_all_types() {
        let (_dir, c) = classified_custom(
            &[
                (".cache/a.mkv", 100),
                (".cache/b.pdf", 100),
                (".cache/c.mp3", 100),
                (".cache/d.png", 100),
            ],
            |home| ClassificationRules::with_xdg(home, &test_xdg(home)),
        );
        assert_eq!(bytes_of(&c, Category::Temporary), 400);
        assert_eq!(bytes_of(&c, Category::Videos), 0);
        assert_eq!(bytes_of(&c, Category::Documents), 0);
    }

    /// 6. Trash absorbs arbitrary file types.
    #[test]
    fn trash_absorbs_all_types() {
        let (_dir, c) = classified_custom(
            &[
                (".local/share/Trash/a.mkv", 100),
                (".local/share/Trash/b.pdf", 100),
                (".local/share/Trash/c.mp3", 100),
                (".local/share/Trash/d.png", 100),
            ],
            |home| ClassificationRules::with_xdg(home, &test_xdg(home)),
        );
        assert_eq!(bytes_of(&c, Category::Trash), 400);
        assert_eq!(bytes_of(&c, Category::Videos), 0);
        assert_eq!(bytes_of(&c, Category::Other), 0);
    }

    /// 7. Prefix collisions must not claim siblings: component boundaries,
    ///    never string prefixes — at depth 1 and nested alike.
    #[test]
    fn prefix_collisions_stay_separate() {
        let (_dir, c) = classified_custom(
            &[
                ("Downloads/a.dat", 10),
                ("Downloads-old/b.dat", 20),
                ("Videos/c.dat", 30),
                ("Videos-old/d.dat", 40),
                (".local/share/Trash/f", 50),
                (".local/share-old/x.dat", 60),
            ],
            |home| ClassificationRules::with_xdg(home, &test_xdg(home)),
        );
        assert_eq!(bytes_of(&c, Category::Downloads), 10);
        assert_eq!(bytes_of(&c, Category::Videos), 30);
        assert_eq!(bytes_of(&c, Category::Trash), 50);
        // -old siblings are unowned: 20 + 40 + 60.
        assert_eq!(bytes_of(&c, Category::Other), 120);
    }

    /// 8. Unknown nested content anywhere lands in Other, exactly.
    #[test]
    fn unknown_nested_content_is_other() {
        let (_dir, c) =
            classified_custom(&[("a/b/c/d.dat", 100), (".hidden/x/y.dat", 200)], |home| {
                ClassificationRules::with_xdg(home, &test_xdg(home))
            });
        assert_eq!(bytes_of(&c, Category::Other), 300);
    }

    /// 9a. Ancestor-descendant tracked overlap: ancestor claimed first
    ///     consumes the whole subtree; the descendant skips with a note.
    #[test]
    fn overlapping_nested_rules_skip_safely() {
        let (_dir, c) = classified_custom(
            &[
                ("pkg/share/flatpak/a.dat", 600),
                ("pkg/share/other.dat", 200),
            ],
            |home| {
                ClassificationRules::with_parts(
                    home,
                    vec![],
                    vec![
                        (
                            home.join("pkg/share"),
                            Category::Applications,
                            "test ancestor",
                        ),
                        (
                            home.join("pkg/share/flatpak"),
                            Category::Applications,
                            "test descendant",
                        ),
                    ],
                )
            },
        );
        // Ancestor consumed 800 including the descendant; counting the
        // descendant again would report 1400 for 800 real bytes.
        assert_eq!(bytes_of(&c, Category::Applications), 800);
        assert_eq!(bytes_of(&c, Category::Other), 0);
        assert!(
            c.notes
                .iter()
                .any(|n| n.contains("overlapping rule skipped")),
            "skip must be visible, notes={:?}",
            c.notes
        );
    }

    /// 9b. Same path claimed by two categories: first wins, bytes counted
    ///     once, remainder still exact.
    #[test]
    fn same_path_two_categories_first_wins() {
        let (_dir, c) = classified_custom(&[("pkg/share/f.dat", 700)], |home| {
            ClassificationRules::with_parts(
                home,
                vec![],
                vec![
                    (home.join("pkg/share"), Category::Trash, "test first claim"),
                    (
                        home.join("pkg/share"),
                        Category::Applications,
                        "test duplicate claim",
                    ),
                ],
            )
        });
        assert_eq!(bytes_of(&c, Category::Trash), 700);
        assert_eq!(bytes_of(&c, Category::Applications), 0);
    }

    /// 10. Full integer-byte equation on a deliberately mixed tree.
    #[test]
    fn integer_byte_equation() {
        let (_dir, c) = classified_custom(
            &[
                ("Documents/a.pdf", 1000),
                ("Downloads/m.mkv", 2000),
                (".cache/c.dat", 4000),
                (".local/share/Trash/t", 8000),
                (".local/share/flatpak/f", 16000),
                (".local/left.dat", 32000),
                ("loose.mp3", 64),
                ("mystery", 36),
            ],
            |home| ClassificationRules::with_xdg(home, &test_xdg(home)),
        );
        // Hand-computed, integer bytes, no rounding anywhere.
        assert_eq!(c.total_bytes, 63100);
        assert_eq!(bytes_of(&c, Category::Documents), 1000);
        assert_eq!(bytes_of(&c, Category::Downloads), 2000);
        assert_eq!(bytes_of(&c, Category::Temporary), 4000);
        assert_eq!(bytes_of(&c, Category::Trash), 8000);
        assert_eq!(bytes_of(&c, Category::Applications), 16000);
        assert_eq!(bytes_of(&c, Category::Music), 64);
        assert_eq!(bytes_of(&c, Category::Videos), 0);
        assert_eq!(bytes_of(&c, Category::Pictures), 0);
        // .local remainder (32000) + extensionless loose file (36).
        assert_eq!(bytes_of(&c, Category::Other), 32036);
        assert_eq!(
            c.category_sum_bytes(),
            1000 + 2000 + 4000 + 8000 + 16000 + 64 + 32036
        );
        assert_eq!(c.category_sum_bytes(), c.total_bytes);
        assert!(c.partition_ok());
    }
    /// 11. Physical used invariant: physical_used == sum(top_level_categories)
    ///     and Other is the exact residual (other == physical_used - identified_categories).
    #[test]
    fn test_physical_accounting_invariant_sum_categories_and_other_residual() {
        let (_dir, c) = classified_custom(
            &[
                ("Documents/report.pdf", 5000),
                ("Downloads/setup.tar", 12000),
                (".cache/app_cache", 3000),
                (".local/share/Trash/item", 1500),
                (".local/share/flatpak/app", 25000),
                (".config/settings.json", 800),
                (".cargo/config", 1200),
                ("workspace/main.rs", 400),
            ],
            |home| ClassificationRules::with_xdg(home, &test_xdg(home)),
        );

        let physical_used = c.total_bytes;
        let sum_categories = c.category_sum_bytes();
        assert_eq!(
            physical_used, sum_categories,
            "physical_used must equal sum(categories)"
        );
        assert!(c.partition_ok());

        // Other is the exact residual of physical_used minus all identified categories
        let mut identified: u64 = 0;
        for cat in Category::ALL {
            if cat != Category::Other {
                identified += bytes_of(&c, cat);
            }
        }
        let other_bytes = bytes_of(&c, Category::Other);
        assert_eq!(
            other_bytes,
            physical_used - identified,
            "other must equal physical_used - identified"
        );
    }

    /// 12. Nested ~/.config child subtraction: browser profiles and developer data
    ///     inside ~/.config are claimed and subtracted from ~/.config's remainder.
    #[test]
    fn test_nested_config_browser_and_developer_claim_subtraction() {
        let (_dir, c) = classified_custom(
            &[
                (".config/google-chrome/Default/History", 3000),
                (".config/Code/User/settings.json", 1500),
                (".config/general/config.ini", 500),
            ],
            |home| ClassificationRules::with_xdg(home, &test_xdg(home)),
        );

        let other = c.of(Category::Other);
        // Total other bytes must be 3000 + 1500 + 500 = 5000
        assert_eq!(other.bytes, 5000);

        // Invariant: sum of individual contributions in Other must not exceed or double-count 5000
        let contrib_sum: u64 = other.contributions.iter().map(|c| c.bytes).sum();
        assert_eq!(contrib_sum, 5000);

        // Find .config parent remainder contribution
        let config_contrib = other
            .contributions
            .iter()
            .find(|c| c.path.ends_with(".config"))
            .expect(".config contribution exists");
        // .config remainder must only be 500, NOT 5000!
        assert_eq!(config_contrib.bytes, 500);

        // The nested claims must be in claimed_away of .config's scope
        assert!(
            config_contrib
                .scope
                .claimed_away
                .iter()
                .any(|p| p.ends_with("google-chrome"))
        );
        assert!(
            config_contrib
                .scope
                .claimed_away
                .iter()
                .any(|p| p.ends_with("Code"))
        );
    }
}
