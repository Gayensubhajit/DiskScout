use std::fs;
use std::path::{Path, PathBuf};

use super::desktop::parse_desktop_file;
use super::model::{Application, ApplicationKind, ApplicationSource, InstallScope};

/// Scan installed Flatpak applications and runtimes.
pub fn scan_flatpak_installations(
    system_root: Option<&Path>,
    user_root: Option<&Path>,
) -> Vec<Application> {
    let mut apps = Vec::new();

    // System Flatpaks (usually /var/lib/flatpak)
    if let Some(sys_path) = system_root {
        scan_flatpak_root(sys_path, InstallScope::System, &mut apps);
    } else {
        let default_sys = Path::new("/var/lib/flatpak");
        if default_sys.is_dir() {
            scan_flatpak_root(default_sys, InstallScope::System, &mut apps);
        }
    }

    // User Flatpaks (usually ~/.local/share/flatpak)
    if let Some(usr_path) = user_root {
        scan_flatpak_root(usr_path, InstallScope::User, &mut apps);
    } else if let Ok(home) = std::env::var("HOME") {
        let default_user = PathBuf::from(home).join(".local/share/flatpak");
        if default_user.is_dir() {
            scan_flatpak_root(&default_user, InstallScope::User, &mut apps);
        }
    }

    apps
}

fn scan_flatpak_root(root: &Path, scope: InstallScope, apps: &mut Vec<Application>) {
    let app_dir = root.join("app");
    if app_dir.is_dir() {
        scan_flatpak_apps_dir(&app_dir, scope, ApplicationKind::Application, apps);
    }

    let runtime_dir = root.join("runtime");
    if runtime_dir.is_dir() {
        scan_flatpak_apps_dir(&runtime_dir, scope, ApplicationKind::Runtime, apps);
    }
}

fn scan_flatpak_apps_dir(
    dir: &Path,
    scope: InstallScope,
    kind: ApplicationKind,
    apps: &mut Vec<Application>,
) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };

    for entry in entries.flatten() {
        let app_id_path = entry.path();
        if !app_id_path.is_dir() {
            continue;
        }

        let app_id = entry.file_name().to_string_lossy().to_string();

        // Standard Flatpak hierarchy: <app_id>/<arch>/<branch>/active/
        if let Some(active_dir) = find_active_deployment(&app_id_path) {
            let (display_name, version, description, icon) =
                inspect_flatpak_deployment(&active_dir, &app_id);
            let size = calculate_dir_size(&active_dir);

            apps.push(Application {
                id: format!("flatpak:{}", app_id),
                name: app_id.clone(),
                display_name,
                version,
                description,
                source: ApplicationSource::Flatpak,
                kind,
                install_scope: scope,
                package_size: size,
                measured_size: Some(size),
                user_data_size: None,
                runtime_size: None,
                location: Some(active_dir),
                desktop_file: None,
                icon_name: icon.unwrap_or_else(|| "package".to_string()),
                is_explicit: true,
            });
        }
    }
}

/// Find the `active` deployment directory or the most recent commit directory.
fn find_active_deployment(app_id_dir: &Path) -> Option<PathBuf> {
    // Check current symlink: <app_id>/current/active
    let current_active = app_id_dir.join("current").join("active");
    if current_active.is_dir() {
        return Some(current_active);
    }

    // Traverse arch/branch/active
    let Ok(arch_entries) = fs::read_dir(app_id_dir) else {
        return None;
    };

    for arch_entry in arch_entries.flatten() {
        let arch_path = arch_entry.path();
        if !arch_path.is_dir() || arch_entry.file_name() == "current" {
            continue;
        }
        let Ok(branch_entries) = fs::read_dir(&arch_path) else {
            continue;
        };
        for branch_entry in branch_entries.flatten() {
            let branch_path = branch_entry.path();
            if !branch_path.is_dir() {
                continue;
            }
            let active = branch_path.join("active");
            if active.is_dir() {
                return Some(active);
            }
        }
    }

    None
}

/// Extract display name, version, description, and icon from active deployment.
fn inspect_flatpak_deployment(
    active_dir: &Path,
    app_id: &str,
) -> (String, String, String, Option<String>) {
    let mut display_name = app_id.to_string();
    let version = String::new();
    let mut description = String::new();
    let mut icon = None;

    // 1. Check exported desktop file: active/export/share/applications/*.desktop
    let export_apps = active_dir.join("export/share/applications");
    if export_apps.is_dir() {
        if let Ok(entries) = fs::read_dir(&export_apps) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.extension().and_then(|e| e.to_str()) == Some("desktop") {
                    if let Some(de) = parse_desktop_file(&p) {
                        display_name = de.name;
                        if let Some(c) = de.comment {
                            description = c;
                        }
                        icon = de.icon;
                        break;
                    }
                }
            }
        }
    }

    // 2. Check metadata file for runtime/command
    let metadata_path = active_dir.join("metadata");
    if metadata_path.is_file() {
        if let Ok(meta_content) = fs::read_to_string(&metadata_path) {
            for line in meta_content.lines() {
                let trimmed = line.trim();
                if let Some(v) = trimmed.strip_prefix("runtime=") {
                    if description.is_empty() {
                        description = format!("Runtime: {}", v);
                    }
                }
            }
        }
    }

    // Clean up default display name if still using domain-reversed ID e.g. org.videolan.VLC -> VLC
    if display_name == app_id {
        if let Some(last_seg) = app_id.split('.').last() {
            if !last_seg.is_empty() {
                display_name = last_seg.to_string();
            }
        }
    }

    (display_name, version, description, icon)
}

/// Calculate shallow directory size for an active Flatpak deployment.
fn calculate_dir_size(dir: &Path) -> u64 {
    let mut total = 0u64;
    // Inspect files directory
    let files_dir = dir.join("files");
    let target = if files_dir.is_dir() { &files_dir } else { dir };

    for entry in walkdir::WalkDir::new(target)
        .max_depth(4)
        .into_iter()
        .flatten()
    {
        if let Ok(meta) = entry.metadata() {
            if meta.is_file() {
                total += meta.len();
            }
        }
    }
    total
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_scan_flatpak_apps_and_runtimes() {
        let dir = tempdir().unwrap();
        let flatpak_root = dir.path();

        // Create an app: org.videolan.VLC
        let vlc_active = flatpak_root.join("app/org.videolan.VLC/x86_64/stable/active");
        fs::create_dir_all(&vlc_active).unwrap();
        let export_dir = vlc_active.join("export/share/applications");
        fs::create_dir_all(&export_dir).unwrap();
        fs::write(
            export_dir.join("org.videolan.VLC.desktop"),
            "[Desktop Entry]
Name=VLC media player
Comment=Play audio and video files
Icon=vlc
",
        )
        .unwrap();
        fs::write(
            vlc_active.join("metadata"),
            "[Application]
name=org.videolan.VLC
runtime=org.kde.Platform/x86_64/5.15
",
        )
        .unwrap();
        let files_dir = vlc_active.join("files");
        fs::create_dir_all(&files_dir).unwrap();
        fs::write(files_dir.join("vlc_binary"), vec![0u8; 100_000]).unwrap();

        // Create a runtime: org.kde.Platform
        let kde_active = flatpak_root.join("runtime/org.kde.Platform/x86_64/5.15/active");
        fs::create_dir_all(&kde_active).unwrap();
        fs::write(
            kde_active.join("metadata"),
            "[Runtime]
name=org.kde.Platform
",
        )
        .unwrap();

        let apps = scan_flatpak_installations(Some(flatpak_root), None);
        assert_eq!(apps.len(), 2);

        let vlc = apps
            .iter()
            .find(|a| a.name == "org.videolan.VLC")
            .expect("VLC app found");
        assert_eq!(vlc.display_name, "VLC media player");
        assert_eq!(vlc.kind, ApplicationKind::Application);
        assert_eq!(vlc.source, ApplicationSource::Flatpak);
        assert!(vlc.package_size >= 100_000);

        let kde = apps
            .iter()
            .find(|a| a.name == "org.kde.Platform")
            .expect("KDE runtime found");
        assert_eq!(kde.kind, ApplicationKind::Runtime);
        assert_eq!(kde.source, ApplicationSource::Flatpak);
    }
}
