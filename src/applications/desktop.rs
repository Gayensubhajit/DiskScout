use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesktopEntry {
    pub file_name: String,
    pub path: PathBuf,
    pub name: String,
    pub generic_name: Option<String>,
    pub comment: Option<String>,
    pub exec: Option<String>,
    pub icon: Option<String>,
    pub no_display: bool,
    pub categories: Vec<String>,
}

#[derive(Debug, Default)]
pub struct DesktopIndex {
    /// Map from desktop file name (e.g. "Alacritty.desktop", lowercase "alacritty.desktop") to DesktopEntry
    pub by_filename: HashMap<String, DesktopEntry>,
    /// Map from binary/command name (e.g. "alacritty", "brave") to DesktopEntry
    pub by_exec: HashMap<String, DesktopEntry>,
}

impl DesktopIndex {
    pub fn new() -> Self {
        Self::default()
    }

    /// Load desktop entries from standard system and user locations.
    pub fn scan_system_and_user() -> Self {
        let mut index = Self::new();
        let paths = [
            PathBuf::from("/usr/share/applications"),
            PathBuf::from("/usr/local/share/applications"),
        ];

        for p in &paths {
            if p.is_dir() {
                index.scan_dir(p);
            }
        }

        if let Ok(home) = std::env::var("HOME") {
            let user_apps = PathBuf::from(home).join(".local/share/applications");
            if user_apps.is_dir() {
                index.scan_dir(&user_apps);
            }
        }

        index
    }

    /// Scan a specific directory for `.desktop` files.
    pub fn scan_dir(&mut self, dir: &Path) {
        let Ok(entries) = fs::read_dir(dir) else {
            return;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) == Some("desktop") {
                if let Some(entry_data) = parse_desktop_file(&path) {
                    let fname = entry_data.file_name.clone();
                    let lower_fname = fname.to_lowercase();

                    if let Some(exec) = &entry_data.exec {
                        let exec_cmd = exec.split_whitespace().next().unwrap_or("").to_lowercase();
                        let base_exec = Path::new(&exec_cmd)
                            .file_name()
                            .and_then(|f| f.to_str())
                            .unwrap_or(&exec_cmd)
                            .to_string();
                        if !base_exec.is_empty() {
                            self.by_exec.insert(base_exec, entry_data.clone());
                        }
                    }

                    self.by_filename.insert(lower_fname, entry_data.clone());
                    self.by_filename.insert(fname, entry_data);
                }
            }
        }
    }

    /// Look up a desktop entry by filename or binary command.
    pub fn find(&self, query: &str) -> Option<&DesktopEntry> {
        let q_lower = query.to_lowercase();
        self.by_filename
            .get(&q_lower)
            .or_else(|| self.by_filename.get(&format!("{q_lower}.desktop")))
            .or_else(|| self.by_exec.get(&q_lower))
    }
}

/// Parse a single `.desktop` file.
pub fn parse_desktop_file(path: &Path) -> Option<DesktopEntry> {
    let content = fs::read_to_string(path).ok()?;
    let file_name = path.file_name()?.to_str()?.to_string();

    let mut in_desktop_entry = false;
    let mut name = None;
    let mut generic_name = None;
    let mut comment = None;
    let mut exec = None;
    let mut icon = None;
    let mut no_display = false;
    let mut categories = Vec::new();

    for line in content.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            in_desktop_entry = line == "[Desktop Entry]";
            continue;
        }

        if !in_desktop_entry || line.starts_with('#') {
            continue;
        }

        let Some((k, v)) = line.split_once('=') else {
            continue;
        };

        let key = k.trim();
        let val = v.trim();

        match key {
            "Name" if name.is_none() => name = Some(val.to_string()),
            "GenericName" if generic_name.is_none() => generic_name = Some(val.to_string()),
            "Comment" if comment.is_none() => comment = Some(val.to_string()),
            "Exec" if exec.is_none() => exec = Some(val.to_string()),
            "Icon" if icon.is_none() => icon = Some(val.to_string()),
            "NoDisplay" => no_display = val.eq_ignore_ascii_case("true"),
            "Categories" => {
                categories = val
                    .split(';')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
            }
            _ => {}
        }
    }

    let name = name.or_else(|| {
        path.file_stem()
            .and_then(|s| s.to_str())
            .map(|s| s.to_string())
    })?;

    Some(DesktopEntry {
        file_name,
        path: path.to_path_buf(),
        name,
        generic_name,
        comment,
        exec,
        icon,
        no_display,
        categories,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_parse_desktop_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test.desktop");
        fs::write(
            &path,
            r#"[Desktop Entry]
Name=Test Application
GenericName=Text Editor
Comment=An awesome text editor
Exec=/usr/bin/test-app %U
Icon=test-icon
Terminal=false
Categories=Utility;TextEditor;
"#,
        )
        .unwrap();

        let entry = parse_desktop_file(&path).expect("valid desktop file");
        assert_eq!(entry.name, "Test Application");
        assert_eq!(entry.generic_name.as_deref(), Some("Text Editor"));
        assert_eq!(entry.comment.as_deref(), Some("An awesome text editor"));
        assert_eq!(entry.exec.as_deref(), Some("/usr/bin/test-app %U"));
        assert_eq!(entry.icon.as_deref(), Some("test-icon"));
        assert!(!entry.no_display);
        assert_eq!(entry.categories, vec!["Utility", "TextEditor"]);
    }

    #[test]
    fn test_desktop_index() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("alacritty.desktop");
        fs::write(
            &path,
            r#"[Desktop Entry]
Name=Alacritty
Exec=alacritty
Icon=Alacritty
"#,
        )
        .unwrap();

        let mut index = DesktopIndex::new();
        index.scan_dir(dir.path());

        assert!(index.find("alacritty").is_some());
        assert!(index.find("alacritty.desktop").is_some());
        assert_eq!(index.find("alacritty").unwrap().name, "Alacritty");
    }
}
