use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

/// FreeDesktop-compatible Provider Icon Resolver with index and memory cache.
#[derive(Debug, Clone, Default)]
pub struct IconResolver {
    cache: HashMap<String, Option<PathBuf>>,
    index: HashMap<String, (u32, PathBuf)>,
    custom_roots: Option<Vec<PathBuf>>,
}

impl IconResolver {
    pub fn new() -> Self {
        let mut resolver = Self {
            cache: HashMap::new(),
            index: HashMap::new(),
            custom_roots: None,
        };
        resolver.build_provider_index();
        resolver
    }

    /// Create an IconResolver with custom search roots (useful for unit tests with fixtures).
    pub fn with_roots(roots: Vec<PathBuf>) -> Self {
        let mut resolver = Self {
            cache: HashMap::new(),
            index: HashMap::new(),
            custom_roots: Some(roots.clone()),
        };
        resolver.index_custom_roots(&roots);
        resolver
    }

    /// Build index of real provider icons from system and user directories.
    fn build_provider_index(&mut self) {
        let provider_dirs: Vec<(&str, u32)> = vec![
            // 1. Fallback system themes (Papirus-Dark, Breeze, Adwaita) - lowest priority (score 10)
            ("/usr/share/icons/Papirus-Dark/64x64/apps", 10),
            ("/usr/share/icons/Papirus/64x64/apps", 10),
            ("/usr/share/icons/breeze/apps/48", 10),
            ("/usr/share/icons/Adwaita/scalable/apps", 10),
            ("/usr/share/icons/Adwaita/48x48/apps", 10),
            // 2. Real provider icons from Flatpak exports
            (
                "/var/lib/flatpak/exports/share/icons/hicolor/scalable/apps",
                1000,
            ),
            (
                "/var/lib/flatpak/exports/share/icons/hicolor/128x128/apps",
                128,
            ),
            (
                "/var/lib/flatpak/exports/share/icons/hicolor/64x64/apps",
                64,
            ),
            (
                "/var/lib/flatpak/exports/share/icons/hicolor/48x48/apps",
                48,
            ),
            // 3. Real provider icons from FreeDesktop hicolor (where apps install official logos)
            ("/usr/share/icons/hicolor/apps", 100),
            ("/usr/share/icons/hicolor/32x32/apps", 32),
            ("/usr/share/icons/hicolor/48x48/apps", 48),
            ("/usr/share/icons/hicolor/64x64/apps", 64),
            ("/usr/share/icons/hicolor/128x128/apps", 128),
            ("/usr/share/icons/hicolor/256x256/apps", 256),
            ("/usr/share/icons/hicolor/512x512/apps", 512),
            ("/usr/share/icons/hicolor/scalable/apps", 1000),
            // 4. Pixmaps (where VS Code, Steam, Alacritty install their official icons)
            ("/usr/share/pixmaps", 200),
        ];

        for (dir_str, base_score) in provider_dirs {
            let p = Path::new(dir_str);
            if p.is_dir() {
                self.index_dir(p, base_score);
            }
        }

        // User icons
        if let Ok(home) = std::env::var("HOME") {
            let h = PathBuf::from(home);
            let user_dirs: Vec<(PathBuf, u32)> = vec![
                (
                    h.join(".local/share/flatpak/exports/share/icons/hicolor/scalable/apps"),
                    1000,
                ),
                (
                    h.join(".local/share/flatpak/exports/share/icons/hicolor/128x128/apps"),
                    128,
                ),
                (h.join(".local/share/icons/hicolor/scalable/apps"), 1000),
                (h.join(".local/share/icons/hicolor/256x256/apps"), 256),
                (h.join(".local/share/icons/hicolor/128x128/apps"), 128),
                (h.join(".local/share/icons/hicolor/64x64/apps"), 64),
                (h.join(".local/share/icons/hicolor/48x48/apps"), 48),
            ];
            for (p, score) in user_dirs {
                if p.is_dir() {
                    self.index_dir(&p, score);
                }
            }
        }
    }

    /// Index a single directory icons.
    fn index_dir(&mut self, dir: &Path, base_score: u32) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if !path.is_file() {
                continue;
            }
            let Some(file_name) = path.file_name().and_then(|f| f.to_str()) else {
                continue;
            };
            if !file_name.ends_with(".svg")
                && !file_name.ends_with(".png")
                && !file_name.ends_with(".xpm")
            {
                continue;
            }
            let path_str = path.to_string_lossy().to_lowercase();
            if path_str.contains("candy")
                || path_str.contains("sweet")
                || path_str.contains("beauty")
            {
                continue;
            }
            let stem = strip_icon_extension(file_name).to_lowercase();
            let is_svg = file_name.ends_with(".svg");
            let score = base_score + (if is_svg { 50 } else { 0 });

            match self.index.get(&stem) {
                Some(&(cur_score, _)) if cur_score >= score => {}
                _ => {
                    self.index.insert(stem, (score, path));
                }
            }
        }
    }

    /// Index custom roots (e.g. from tests).
    fn index_custom_roots(&mut self, roots: &[PathBuf]) {
        for root in roots {
            if root.is_dir() {
                self.index_dir_recursive(root, 10);
            }
        }
    }

    fn index_dir_recursive(&mut self, dir: &Path, depth: u32) {
        if depth == 0 {
            return;
        }
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_file() {
                if let Some(name) = path.file_name().and_then(|f| f.to_str()) {
                    if name.ends_with(".svg") || name.ends_with(".png") || name.ends_with(".xpm") {
                        let stem = strip_icon_extension(name).to_lowercase();
                        self.index.entry(stem).or_insert((100, path));
                    }
                }
            } else if path.is_dir() {
                self.index_dir_recursive(&path, depth - 1);
            }
        }
    }

    /// Resolve an icon name or path to an absolute filesystem PathBuf.
    pub fn resolve_path(&mut self, icon_name: &str) -> Option<PathBuf> {
        let trimmed = icon_name.trim();
        if trimmed.is_empty() {
            return None;
        }

        if let Some(cached) = self.cache.get(trimmed) {
            return cached.clone();
        }

        let resolved = self.find_icon_internal(trimmed);
        self.cache.insert(trimmed.to_string(), resolved.clone());
        resolved
    }

    /// Check if the path for an icon name has been resolved in the path cache.
    pub fn is_path_cached(&self, icon_name: &str) -> bool {
        self.cache.contains_key(icon_name.trim())
    }

    /// Resolve an icon name to a Slint Image.
    pub fn resolve_image(&mut self, icon_name: &str) -> Option<slint::Image> {
        let path = self.resolve_path(icon_name)?;
        slint::Image::load_from_path(&path).ok()
    }

    /// Pre-warm the cache by resolving all icon paths on a background thread.
    /// This avoids doing filesystem lookups on the UI thread.
    pub fn prewarm(&mut self, icon_names: &[&str]) {
        for name in icon_names {
            self.resolve_path(name);
        }
    }

    fn find_icon_internal(&self, icon_name: &str) -> Option<PathBuf> {
        // 1. Direct absolute path
        let direct_path = Path::new(icon_name);
        if direct_path.is_absolute() && direct_path.is_file() {
            let path_str = direct_path.to_string_lossy().to_lowercase();
            if !path_str.contains("candy")
                && !path_str.contains("sweet")
                && !path_str.contains("beauty")
            {
                return Some(direct_path.to_path_buf());
            }
        }

        let stripped = strip_icon_extension(icon_name);
        let lower = stripped.to_lowercase();

        // 2. Check pre-computed provider index
        if let Some((_, path)) = self.index.get(&lower) {
            return Some(path.clone());
        }

        // 3. Also check with exact case
        if let Some((_, path)) = self.index.get(stripped) {
            return Some(path.clone());
        }

        // 4. Fallback for custom roots (e.g. tests)
        if let Some(roots) = &self.custom_roots {
            for root in roots {
                if let Some(p) = check_candidate_file(root, stripped) {
                    return Some(p);
                }
                let subdirs = [
                    "apps/scalable",
                    "scalable/apps",
                    "128x128@2x/apps",
                    "128x128/apps",
                    "64x64/apps",
                    "48x48/apps",
                    "32x32/apps",
                    "256x256/apps",
                    "apps",
                ];
                for sub in &subdirs {
                    let sub_path = root.join(sub);
                    if sub_path.is_dir() {
                        if let Some(p) = check_candidate_file(&sub_path, stripped) {
                            return Some(p);
                        }
                    }
                }
            }
        }

        None
    }
}

fn strip_icon_extension(name: &str) -> &str {
    if let Some(s) = name.strip_suffix(".svg") {
        s
    } else if let Some(s) = name.strip_suffix(".png") {
        s
    } else if let Some(s) = name.strip_suffix(".xpm") {
        s
    } else {
        name
    }
}

fn check_candidate_file(dir: &Path, base_name: &str) -> Option<PathBuf> {
    for ext in &[".svg", ".png", ".xpm", ""] {
        let cand = dir.join(format!("{base_name}{ext}"));
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

/// UI-thread decoded Slint Image cache.
/// Because Image is not Send, this cache lives on the UI thread (e.g. Rc<RefCell<UiImageCache>>).
#[derive(Default)]
pub struct UiImageCache {
    images: HashMap<PathBuf, slint::Image>,
    /// Paths whose decode already failed. Retried decodes would hit the
    /// disk on every scroll tick, so failures are memoized (M7.2.2).
    failed: std::collections::HashSet<PathBuf>,
}

impl UiImageCache {
    pub fn new() -> Self {
        Self::default()
    }

    /// Retrieve an already-decoded Slint Image if in cache.
    pub fn get(&self, path: &Path) -> Option<slint::Image> {
        self.images.get(path).cloned()
    }

    /// Load from path or return cached image. Failed decodes are memoized
    /// and return `None` immediately on repeat calls.
    pub fn get_or_load(&mut self, path: &Path) -> Option<slint::Image> {
        if let Some(img) = self.images.get(path) {
            return Some(img.clone());
        }
        if self.failed.contains(path) {
            return None;
        }
        if let Ok(img) = slint::Image::load_from_path(path) {
            self.images.insert(path.to_path_buf(), img.clone());
            Some(img)
        } else {
            self.failed.insert(path.to_path_buf());
            None
        }
    }

    /// Check if an image is already cached.
    pub fn contains(&self, path: &Path) -> bool {
        self.images.contains_key(path)
    }

    /// Check if a path already failed to decode (memoized, no disk hit).
    pub fn is_failed(&self, path: &Path) -> bool {
        self.failed.contains(path)
    }

    /// Number of cached decoded images.
    pub fn len(&self) -> usize {
        self.images.len()
    }

    pub fn is_empty(&self) -> bool {
        self.images.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_absolute_icon_path() {
        let dir = tempdir().unwrap();
        let icon_file = dir.path().join("app.png");
        fs::write(&icon_file, b"fake png data").unwrap();

        let mut resolver = IconResolver::new();
        let resolved = resolver.resolve_path(icon_file.to_str().unwrap());
        assert_eq!(resolved, Some(icon_file));
    }

    #[test]
    fn test_user_icon_directory_resolution() {
        let dir = tempdir().unwrap();
        let user_icons = dir.path().join(".local/share/icons/custom");
        fs::create_dir_all(user_icons.join("scalable/apps")).unwrap();
        let icon_file = user_icons.join("scalable/apps/custom-app.svg");
        fs::write(&icon_file, b"<svg></svg>").unwrap();

        let mut resolver = IconResolver::with_roots(vec![user_icons]);
        let resolved = resolver.resolve_path("custom-app");
        assert_eq!(resolved, Some(icon_file));
    }

    #[test]
    fn test_system_icon_directory_and_hicolor_fallback() {
        let dir = tempdir().unwrap();
        let hicolor = dir.path().join("hicolor/48x48/apps");
        fs::create_dir_all(&hicolor).unwrap();
        let hicolor_icon = hicolor.join("test-app.png");
        fs::write(&hicolor_icon, b"png").unwrap();

        let mut resolver = IconResolver::with_roots(vec![dir.path().join("hicolor")]);
        let resolved = resolver.resolve_path("test-app");
        assert_eq!(resolved, Some(hicolor_icon));
    }

    #[test]
    fn test_missing_icon_returns_none() {
        let dir = tempdir().unwrap();
        let mut resolver = IconResolver::with_roots(vec![dir.path().to_path_buf()]);
        let resolved = resolver.resolve_path("non-existent-icon-12345");
        assert!(resolved.is_none());
    }

    #[test]
    fn test_two_level_icon_cache_hit_and_miss() {
        let dir = tempdir().unwrap();
        let mut resolver = IconResolver::with_roots(vec![dir.path().to_path_buf()]);
        let img_cache = UiImageCache::new();

        // Non-existent icon -> None path
        assert!(!resolver.is_path_cached("missing"));
        assert!(resolver.resolve_path("missing").is_none());
        assert!(resolver.is_path_cached("missing"));

        // Valid icon
        let icon_file = dir.path().join("valid.png");
        std::fs::write(&icon_file, b"fake png data").unwrap();
        let mut resolver_valid = IconResolver::with_roots(vec![dir.path().to_path_buf()]);
        let path = resolver_valid
            .resolve_path("valid")
            .expect("should resolve path");
        assert_eq!(path, icon_file);
        assert!(!img_cache.contains(&path));
    }

    #[test]
    fn test_cached_resolution_returns_instantly() {
        let dir = tempdir().unwrap();
        let icon_dir = dir.path().join("icons");
        fs::create_dir_all(&icon_dir).unwrap();
        let icon_file = icon_dir.join("cached-app.svg");
        fs::write(&icon_file, b"<svg></svg>").unwrap();

        let mut resolver = IconResolver::with_roots(vec![icon_dir.clone()]);
        let r1 = resolver.resolve_path("cached-app");
        assert_eq!(r1, Some(icon_file.clone()));

        // Remove the file on disk to prove second lookup is served from memory cache
        fs::remove_file(&icon_file).unwrap();
        let r2 = resolver.resolve_path("cached-app");
        assert_eq!(r2, Some(icon_file));
    }
}
