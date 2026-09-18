use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use flate2::read::GzDecoder;
use tar::Archive;

use super::desktop::DesktopIndex;
use super::model::{Application, ApplicationKind, ApplicationSource, InstallScope};

/// Scan native and foreign packages from the local pacman database.
pub fn scan_pacman_packages(
    local_db_path: &Path,
    sync_db_path: Option<&Path>,
    desktop_index: &DesktopIndex,
) -> Vec<Application> {
    let Ok(entries) = fs::read_dir(local_db_path) else {
        return Vec::new();
    };

    // Load native sync database package names to distinguish Native vs Foreign
    let native_names = sync_db_path
        .map(load_sync_package_names)
        .unwrap_or_default();
    let has_sync_data = !native_names.is_empty();

    let mut apps = Vec::new();

    for entry in entries.flatten() {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }

        let desc_path = path.join("desc");
        if !desc_path.is_file() {
            continue;
        }

        let Ok(desc_content) = fs::read_to_string(&desc_path) else {
            continue;
        };

        let Some(pkg) = parse_pacman_desc(&desc_content) else {
            continue;
        };

        // Read files list if available to detect desktop entries & binaries
        let files_path = path.join("files");
        let (desktop_files, has_binaries) = if files_path.is_file() {
            parse_pacman_files(&files_path)
        } else {
            (Vec::new(), false)
        };

        // Determine if package is Native (Pacman) or Foreign
        let is_foreign = if has_sync_data {
            !native_names.contains(&pkg.name)
        } else {
            false
        };

        let source = if is_foreign {
            ApplicationSource::Foreign
        } else {
            ApplicationSource::Pacman
        };

        // Application filtering:
        // 1. Packages providing desktop entries are GUI applications.
        // 2. Explicitly installed packages with binaries are user applications/tools.
        // 3. Foreign packages with binaries or desktop entries are applications.
        let has_desktop = !desktop_files.is_empty();
        let is_application =
            has_desktop || (pkg.is_explicit && has_binaries) || (is_foreign && has_binaries);

        let kind = if has_desktop {
            ApplicationKind::Application
        } else if has_binaries {
            ApplicationKind::Tool
        } else {
            ApplicationKind::Library
        };

        // Only include actual user-facing applications and tools in the inventory
        if !is_application {
            continue;
        }

        // Correlate with DesktopIndex for friendly name and icon
        let mut display_name = pkg.name.clone();
        let mut description = pkg.desc.clone();
        let mut icon_name = "package".to_string();
        let mut matched_desktop = None;

        // Sort desktop files to prefer primary launcher over secondary URL handlers or mime helpers
        let mut sorted_desktops = desktop_files.clone();
        sorted_desktops.sort_by_key(|f| {
            let lower = f.to_lowercase();
            let is_handler = lower.contains("url-handler")
                || lower.contains("open-with")
                || lower.contains("handler");
            let matches_pkg = lower.contains(&pkg.name.to_lowercase());
            (is_handler, !matches_pkg, f.len())
        });

        for df in &sorted_desktops {
            if let Some(de) = desktop_index.find(df) {
                // If the desktop entry specifies NoDisplay=true and we have alternatives, skip
                if de.no_display && sorted_desktops.len() > 1 {
                    continue;
                }
                display_name = de.name.clone();
                if let Some(c) = &de.comment {
                    if !c.is_empty() {
                        description = c.clone();
                    }
                }
                if let Some(ic) = &de.icon {
                    icon_name = ic.clone();
                }
                matched_desktop = Some(de.path.clone());
                break;
            }
        }

        if matched_desktop.is_none() {
            if let Some(de) = desktop_index.find(&pkg.name) {
                display_name = de.name.clone();
                if let Some(c) = &de.comment {
                    if !c.is_empty() {
                        description = c.clone();
                    }
                }
                if let Some(ic) = &de.icon {
                    icon_name = ic.clone();
                }
                matched_desktop = Some(de.path.clone());
            }
        }

        let app_location = if has_binaries {
            Some(PathBuf::from("/usr/bin").join(&pkg.name))
        } else {
            matched_desktop.clone()
        };

        apps.push(Application {
            id: format!("{}:{}", source.as_str().to_lowercase(), pkg.name),
            name: pkg.name,
            display_name,
            version: pkg.version,
            description,
            source,
            kind,
            install_scope: InstallScope::System,
            package_size: pkg.size,
            measured_size: None,
            user_data_size: None,
            runtime_size: None,
            location: app_location,
            desktop_file: matched_desktop,
            icon_name,
            is_explicit: pkg.is_explicit,
        });
    }

    apps
}

#[derive(Debug)]
pub struct PacmanDesc {
    pub name: String,
    pub version: String,
    pub desc: String,
    pub size: u64,
    pub is_explicit: bool,
    pub arch: String,
}

/// Parse a pacman `desc` file content.
pub fn parse_pacman_desc(content: &str) -> Option<PacmanDesc> {
    let mut name = None;
    let mut version = String::new();
    let mut desc = String::new();
    let mut size = 0u64;
    let mut is_explicit = true; // Pacman default when %REASON% is absent is 0 (explicit)
    let mut arch = String::new();

    let mut current_section = "";

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with('%') && trimmed.ends_with('%') {
            current_section = trimmed;
            continue;
        }

        if trimmed.is_empty() {
            continue;
        }

        match current_section {
            "%NAME%" if name.is_none() => name = Some(trimmed.to_string()),
            "%VERSION%" if version.is_empty() => version = trimmed.to_string(),
            "%DESC%" if desc.is_empty() => desc = trimmed.to_string(),
            "%SIZE%" if size == 0 => {
                if let Ok(s) = trimmed.parse::<u64>() {
                    size = s;
                }
            }
            "%REASON%" => {
                // 0 = explicit, 1 = dependency
                if trimmed == "1" {
                    is_explicit = false;
                } else if trimmed == "0" {
                    is_explicit = true;
                }
            }
            "%ARCH%" if arch.is_empty() => arch = trimmed.to_string(),
            _ => {}
        }
    }

    let name = name?;
    Some(PacmanDesc {
        name,
        version,
        desc,
        size,
        is_explicit,
        arch,
    })
}

/// Parse a pacman `files` list file.
/// Returns (list of desktop file names, has_binaries).
pub fn parse_pacman_files(path: &Path) -> (Vec<String>, bool) {
    let Ok(content) = fs::read_to_string(path) else {
        return (Vec::new(), false);
    };

    let mut desktop_files = Vec::new();
    let mut has_binaries = false;

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("usr/share/applications/") && trimmed.ends_with(".desktop") {
            if let Some(file_name) = Path::new(trimmed).file_name().and_then(|f| f.to_str()) {
                desktop_files.push(file_name.to_string());
            }
        }
        if trimmed.starts_with("usr/bin/")
            && trimmed.len() > "usr/bin/".len()
            && !trimmed.ends_with('/')
        {
            has_binaries = true;
        }
    }

    (desktop_files, has_binaries)
}

/// Read native package names from pacman sync databases (`/var/lib/pacman/sync/*.db`).
pub fn load_sync_package_names(sync_dir: &Path) -> HashSet<String> {
    let mut names = HashSet::new();
    let Ok(entries) = fs::read_dir(sync_dir) else {
        return names;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("db") {
            continue;
        }

        let Ok(bytes) = fs::read(&path) else {
            continue;
        };

        if bytes.len() >= 2 && bytes[0] == 0x1f && bytes[1] == 0x8b {
            let cursor = std::io::Cursor::new(bytes);
            let buf_reader = std::io::BufReader::with_capacity(64 * 1024, cursor);
            let decoder = GzDecoder::new(buf_reader);
            let mut archive = Archive::new(decoder);
            if let Ok(members) = archive.entries() {
                for m in members.flatten() {
                    if let Ok(p) = m.path() {
                        if let Some(first_component) = p.components().next() {
                            let comp_str = first_component.as_os_str().to_string_lossy();
                            if let Some(pkg_name) = extract_pkg_name(&comp_str) {
                                names.insert(pkg_name.to_string());
                            }
                        }
                    }
                }
            }
        } else {
            let cursor = std::io::Cursor::new(bytes);
            let mut archive = Archive::new(cursor);
            if let Ok(members) = archive.entries() {
                for m in members.flatten() {
                    if let Ok(p) = m.path() {
                        if let Some(first_component) = p.components().next() {
                            let comp_str = first_component.as_os_str().to_string_lossy();
                            if let Some(pkg_name) = extract_pkg_name(&comp_str) {
                                names.insert(pkg_name.to_string());
                            }
                        }
                    }
                }
            }
        }
    }

    names
}

/// Extract package name from pacman archive entry format `<name>-<pkgver>-<pkgrel>`.
fn extract_pkg_name(dir_name: &str) -> Option<&str> {
    let parts: Vec<&str> = dir_name.split('-').collect();
    if parts.len() >= 3 {
        // Drop last two segments (pkgrel, pkgver)
        let cutoff = dir_name.rfind('-')?;
        let sub = &dir_name[..cutoff];
        let cutoff2 = sub.rfind('-')?;
        Some(&dir_name[..cutoff2])
    } else {
        Some(dir_name)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs::File;
    use tempfile::tempdir;

    #[test]
    fn test_parse_pacman_desc() {
        let desc = r#"%NAME%
alacritty

%VERSION%
0.17.0-1

%DESC%
A cross-platform, GPU-accelerated terminal emulator

%SIZE%
8700781

%REASON%
0

%ARCH%
x86_64
"#;
        let parsed = parse_pacman_desc(desc).unwrap();
        assert_eq!(parsed.name, "alacritty");
        assert_eq!(parsed.version, "0.17.0-1");
        assert_eq!(
            parsed.desc,
            "A cross-platform, GPU-accelerated terminal emulator"
        );
        assert_eq!(parsed.size, 8700781);
        assert!(parsed.is_explicit);
    }

    #[test]
    fn test_parse_dependency_reason() {
        let desc = r#"%NAME%
glibc

%VERSION%
2.41-1

%SIZE%
50000000

%REASON%
1
"#;
        let parsed = parse_pacman_desc(desc).unwrap();
        assert_eq!(parsed.name, "glibc");
        assert!(!parsed.is_explicit);
    }

    #[test]
    fn test_pacman_scanner_identifies_native_and_foreign() {
        let dir = tempdir().unwrap();
        let local_dir = dir.path().join("local");
        let sync_dir = dir.path().join("sync");
        fs::create_dir_all(&local_dir).unwrap();
        fs::create_dir_all(&sync_dir).unwrap();

        // Native package directory: firefox-147.0-1
        let ff_dir = local_dir.join("firefox-147.0-1");
        fs::create_dir_all(&ff_dir).unwrap();
        fs::write(
            ff_dir.join("desc"),
            "%NAME%
firefox
%VERSION%
147.0-1
%DESC%
Web Browser
%SIZE%
482000000
%REASON%
0
",
        )
        .unwrap();
        fs::write(
            ff_dir.join("files"),
            "%FILES%
usr/bin/firefox
usr/share/applications/firefox.desktop
",
        )
        .unwrap();

        // Foreign package directory: brave-bin-1.0-1
        let brave_dir = local_dir.join("brave-bin-1.0-1");
        fs::create_dir_all(&brave_dir).unwrap();
        fs::write(
            brave_dir.join("desc"),
            "%NAME%
brave-bin
%VERSION%
1.0-1
%DESC%
Brave Browser
%SIZE%
350000000
%REASON%
0
",
        )
        .unwrap();
        fs::write(
            brave_dir.join("files"),
            "%FILES%
usr/bin/brave
usr/share/applications/brave-browser.desktop
",
        )
        .unwrap();

        // Dependency package: zlib-1.3-1 (no desktop, reason 1) -> must be filtered out of primary applications
        let zlib_dir = local_dir.join("zlib-1.3-1");
        fs::create_dir_all(&zlib_dir).unwrap();
        fs::write(
            zlib_dir.join("desc"),
            "%NAME%
zlib
%VERSION%
1.3-1
%DESC%
Compression library
%SIZE%
120000
%REASON%
1
",
        )
        .unwrap();
        fs::write(
            zlib_dir.join("files"),
            "%FILES%
usr/lib/libz.so
",
        )
        .unwrap();

        // Create a mock sync db containing only "firefox"
        let sync_tar_path = sync_dir.join("core.db");
        {
            let tar_file = File::create(&sync_tar_path).unwrap();
            let mut enc = flate2::write::GzEncoder::new(tar_file, flate2::Compression::default());
            let mut tar = tar::Builder::new(&mut enc);
            let mut header = tar::Header::new_gnu();
            header.set_path("firefox-147.0-1/desc").unwrap();
            header.set_size(10);
            header.set_cksum();
            tar.append(&header, &b"testdesc12"[..]).unwrap();
            tar.finish().unwrap();
        }

        let desktop_idx = DesktopIndex::new();
        let apps = scan_pacman_packages(&local_dir, Some(&sync_dir), &desktop_idx);

        // zlib should NOT be included
        assert_eq!(apps.len(), 2);

        let ff = apps
            .iter()
            .find(|a| a.name == "firefox")
            .expect("firefox found");
        assert_eq!(ff.source, ApplicationSource::Pacman); // Native
        assert_eq!(ff.package_size, 482000000);

        let brave = apps
            .iter()
            .find(|a| a.name == "brave-bin")
            .expect("brave found");
        assert_eq!(brave.source, ApplicationSource::Foreign); // Foreign
        assert_eq!(brave.package_size, 350000000);
    }
}
