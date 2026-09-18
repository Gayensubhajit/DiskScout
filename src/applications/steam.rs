use std::fs;
use std::path::{Path, PathBuf};

use super::model::{Application, ApplicationKind, ApplicationSource, InstallScope};

/// Scan installed Steam games and the Steam client.
pub fn scan_steam_installations(custom_steam_root: Option<&Path>) -> Vec<Application> {
    let mut apps = Vec::new();

    let steam_roots: Vec<PathBuf> = if let Some(p) = custom_steam_root {
        vec![p.to_path_buf()]
    } else if let Ok(home) = std::env::var("HOME") {
        vec![
            PathBuf::from(&home).join(".local/share/Steam"),
            PathBuf::from(&home).join(".steam/steam"),
        ]
    } else {
        Vec::new()
    };

    let Some(active_root) = steam_roots.into_iter().find(|p| p.is_dir()) else {
        return apps;
    };

    // 1. Steam Client Entry
    apps.push(Application {
        id: "steam:client".to_string(),
        name: "steam".to_string(),
        display_name: "Steam Client".to_string(),
        version: String::new(),
        description: "Digital distribution platform & gaming client".to_string(),
        source: ApplicationSource::Steam,
        kind: ApplicationKind::Application,
        install_scope: InstallScope::User,
        package_size: 0,
        measured_size: None,
        user_data_size: None,
        runtime_size: None,
        location: Some(active_root.clone()),
        desktop_file: None,
        icon_name: "steam".to_string(),
        is_explicit: true,
    });

    // 2. Discover all Steam library folders from libraryfolders.vdf
    let library_paths = discover_steam_libraries(&active_root);

    // 3. Scan games in each library
    for lib_path in library_paths {
        scan_steam_library_apps(&lib_path, &mut apps);
    }

    apps
}

/// Discover all library folders from `steamapps/libraryfolders.vdf`.
fn discover_steam_libraries(steam_root: &Path) -> Vec<PathBuf> {
    let mut libraries = vec![steam_root.to_path_buf()];
    let vdf_path = steam_root.join("steamapps/libraryfolders.vdf");

    let Ok(content) = fs::read_to_string(&vdf_path) else {
        return libraries;
    };

    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("\"path\"") {
            let parts: Vec<&str> = trimmed
                .split('"')
                .filter(|s| !s.trim().is_empty())
                .collect();
            if parts.len() >= 2 {
                let path = PathBuf::from(parts[1]);
                if path.is_dir() && !libraries.contains(&path) {
                    libraries.push(path);
                }
            }
        }
    }

    libraries
}

/// Scan `steamapps/appmanifest_*.acf` files in a given library directory.
fn scan_steam_library_apps(lib_path: &Path, apps: &mut Vec<Application>) {
    let steamapps_dir = lib_path.join("steamapps");
    let Ok(entries) = fs::read_dir(&steamapps_dir) else {
        return;
    };

    for entry in entries.flatten() {
        let path = entry.path();
        let file_name = entry.file_name().to_string_lossy().to_string();

        if file_name.starts_with("appmanifest_") && file_name.ends_with(".acf") {
            if let Some(game) = parse_appmanifest(&path, lib_path) {
                apps.push(game);
            }
        }
    }
}

/// Parse a single `appmanifest_<appid>.acf` file.
pub fn parse_appmanifest(manifest_path: &Path, lib_root: &Path) -> Option<Application> {
    let content = fs::read_to_string(manifest_path).ok()?;

    let mut appid = String::new();
    let mut name = String::new();
    let mut installdir = String::new();
    let mut size_on_disk = 0u64;

    for line in content.lines() {
        let trimmed = line.trim();
        let parts: Vec<&str> = trimmed
            .split('"')
            .filter(|s| !s.trim().is_empty())
            .collect();
        if parts.len() >= 2 {
            match parts[0] {
                "appid" if appid.is_empty() => appid = parts[1].to_string(),
                "name" if name.is_empty() => name = parts[1].to_string(),
                "installdir" if installdir.is_empty() => installdir = parts[1].to_string(),
                "SizeOnDisk" if size_on_disk == 0 => {
                    size_on_disk = parts[1].parse::<u64>().unwrap_or(0);
                }
                _ => {}
            }
        }
    }

    if appid.is_empty() {
        return None;
    }

    // Determine clean display name
    let display_name = if name.starts_with("appid_") || name.is_empty() {
        if !installdir.is_empty() {
            installdir.replace('_', " ")
        } else {
            format!("Steam App {}", appid)
        }
    } else {
        name.clone()
    };

    let game_dir = lib_root.join("steamapps/common").join(&installdir);
    let location = if game_dir.is_dir() {
        Some(game_dir)
    } else {
        Some(manifest_path.to_path_buf())
    };

    let kind = if display_name.contains("Proton")
        || display_name.contains("Redistributable")
        || display_name.contains("Runtime")
    {
        ApplicationKind::Tool
    } else {
        ApplicationKind::Game
    };

    Some(Application {
        id: format!("steam:{}", appid),
        name: if name.is_empty() { appid.clone() } else { name },
        display_name,
        version: format!("AppID: {}", appid),
        description: format!("Steam {}", kind_label(kind)),
        source: ApplicationSource::Steam,
        kind,
        install_scope: InstallScope::User,
        package_size: size_on_disk,
        measured_size: if size_on_disk > 0 {
            Some(size_on_disk)
        } else {
            None
        },
        user_data_size: None,
        runtime_size: None,
        location,
        desktop_file: None,
        icon_name: "steam".to_string(),
        is_explicit: true,
    })
}

fn kind_label(kind: ApplicationKind) -> &'static str {
    match kind {
        ApplicationKind::Game => "Game",
        ApplicationKind::Tool => "Tool / Runtime",
        _ => "Application",
    }
}

/// Measure workshop content size for a Steam app.
/// Checks `steamapps/workshop/content/<app_id>/` in every known Steam library.
/// Returns (total_bytes, library_path_containing_workshop) or None if absent.
pub fn workshop_content_size(app_id: &str) -> Option<u64> {
    let steam_roots: Vec<std::path::PathBuf> = if let Ok(home) = std::env::var("HOME") {
        vec![
            std::path::PathBuf::from(&home).join(".local/share/Steam"),
            std::path::PathBuf::from(&home).join(".steam/steam"),
        ]
    } else {
        return None;
    };

    let active_root = steam_roots.into_iter().find(|p| p.is_dir())?;
    let libraries = discover_steam_libraries(&active_root);

    let mut total = 0u64;
    for lib in &libraries {
        let workshop_dir = lib.join("steamapps/workshop/content").join(app_id);
        if workshop_dir.is_dir() {
            total += dir_size_bytes(&workshop_dir);
        }
    }
    if total > 0 { Some(total) } else { None }
}

/// Recursive directory size (best-effort; skips permission errors).
fn dir_size_bytes(dir: &std::path::Path) -> u64 {
    let Ok(entries) = fs::read_dir(dir) else {
        return 0;
    };
    entries
        .flatten()
        .map(|e| {
            let path = e.path();
            if path.is_dir() {
                dir_size_bytes(&path)
            } else {
                fs::metadata(&path).map(|m| m.len()).unwrap_or(0)
            }
        })
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_parse_appmanifest_and_steam_scan() {
        let dir = tempdir().unwrap();
        let steam_root = dir.path();
        let steamapps = steam_root.join("steamapps");
        fs::create_dir_all(&steamapps).unwrap();

        // Create libraryfolders.vdf
        fs::write(
            steamapps.join("libraryfolders.vdf"),
            format!(
                r#""libraryfolders" {{
    "0" {{
        "path" "{}"
    }}
}}"#,
                steam_root.display()
            ),
        )
        .unwrap();

        // Create appmanifest_431960.acf
        let manifest = r#""AppState"
{
    "appid" "431960"
    "name" "Wallpaper Engine"
    "installdir" "wallpaper_engine"
    "SizeOnDisk" "826275581"
}
"#;
        fs::write(steamapps.join("appmanifest_431960.acf"), manifest).unwrap();

        let apps = scan_steam_installations(Some(steam_root));
        // Steam client + 1 game
        assert_eq!(apps.len(), 2);

        let game = apps
            .iter()
            .find(|a| a.id == "steam:431960")
            .expect("Wallpaper Engine found");
        assert_eq!(game.display_name, "Wallpaper Engine");
        assert_eq!(game.source, ApplicationSource::Steam);
        assert_eq!(game.package_size, 826275581);
        assert_eq!(game.measured_size, Some(826275581));
    }
}
