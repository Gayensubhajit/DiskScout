use std::collections::HashSet;
use std::path::Path;

use super::desktop::DesktopIndex;
use super::flatpak::scan_flatpak_installations;
use super::model::{Application, sort_applications};
use super::pacman::scan_pacman_packages;
use super::steam::scan_steam_installations;

/// Discover all Linux applications across Pacman, Flatpak, and Steam providers.
pub fn discover_linux_applications() -> Vec<Application> {
    let mut all_apps = Vec::new();
    let mut seen_ids = HashSet::new();

    // 1. Build desktop index for human-readable names, icons, categories
    let desktop_index = DesktopIndex::scan_system_and_user();

    // 2. Scan Pacman local database with sync database foreign detection
    let pacman_db = Path::new("/var/lib/pacman/local");
    let sync_db = Path::new("/var/lib/pacman/sync");
    if pacman_db.is_dir() {
        let sync_opt = if sync_db.is_dir() {
            Some(sync_db)
        } else {
            None
        };
        let pacman_apps = scan_pacman_packages(pacman_db, sync_opt, &desktop_index);
        for app in pacman_apps {
            if seen_ids.insert(app.id.clone()) {
                all_apps.push(app);
            }
        }
    }

    // 3. Scan Flatpak installations (System & User)
    let flatpak_apps = scan_flatpak_installations(None, None);
    for app in flatpak_apps {
        if seen_ids.insert(app.id.clone()) {
            all_apps.push(app);
        }
    }

    // 4. Scan Steam client & installed games
    let steam_apps = scan_steam_installations(None);
    for app in steam_apps {
        if seen_ids.insert(app.id.clone()) {
            all_apps.push(app);
        }
    }

    // 5. Default sort: largest package size / measured size first
    sort_applications(&mut all_apps, "size_desc");

    all_apps
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::applications::model::{ApplicationKind, ApplicationSource, InstallScope};

    #[test]
    fn test_duplicate_isolation_and_merging() {
        let mut apps = Vec::new();
        let mut seen = HashSet::new();

        let app1 = Application {
            id: "pacman:firefox".into(),
            name: "firefox".into(),
            display_name: "Firefox".into(),
            version: "147.0".into(),
            description: "Web Browser".into(),
            source: ApplicationSource::Pacman,
            kind: ApplicationKind::Application,
            install_scope: InstallScope::System,
            package_size: 400_000_000,
            measured_size: None,
            user_data_size: None,
            runtime_size: None,
            location: None,
            desktop_file: None,
            icon_name: "firefox".into(),
            is_explicit: true,
        };

        let app2 = app1.clone(); // duplicate

        for a in [app1, app2] {
            if seen.insert(a.id.clone()) {
                apps.push(a);
            }
        }

        assert_eq!(apps.len(), 1);
    }

    #[test]
    fn test_desktop_application_correlation() {
        use std::fs;
        use tempfile::tempdir;

        let dir = tempdir().unwrap();
        let local_dir = dir.path().join("local");
        let pkg_dir = local_dir.join("firefox-147.0-1");
        fs::create_dir_all(&pkg_dir).unwrap();

        fs::write(
            pkg_dir.join("desc"),
            "%NAME%\nfirefox\n%VERSION%\n147.0-1\n%DESC%\nRaw fallback desc\n%SIZE%\n482000000\n%REASON%\n0\n",
        ).unwrap();
        fs::write(
            pkg_dir.join("files"),
            "%FILES%\nusr/bin/firefox\nusr/share/applications/firefox.desktop\n",
        )
        .unwrap();

        let desktop_dir = dir.path().join("applications");
        fs::create_dir_all(&desktop_dir).unwrap();
        fs::write(
            desktop_dir.join("firefox.desktop"),
            "[Desktop Entry]\nName=Mozilla Firefox\nComment=Browse the Web\nIcon=firefox-brand\nExec=firefox %u\n",
        ).unwrap();

        let mut desktop_idx = DesktopIndex::new();
        desktop_idx.scan_dir(&desktop_dir);

        let apps = scan_pacman_packages(&local_dir, None, &desktop_idx);
        assert_eq!(apps.len(), 1);
        let app = &apps[0];
        // Must correlate with desktop file metadata
        assert_eq!(app.display_name, "Mozilla Firefox");
        assert_eq!(app.description, "Browse the Web");
        assert_eq!(app.icon_name, "firefox-brand");
    }

    #[test]
    fn test_provider_failure_isolation() {
        use std::fs;
        use std::path::Path;
        use tempfile::tempdir;

        // 1. Pacman provider points to non-existent directory -> should return empty vec, not panic
        let non_existent = Path::new("/tmp/diskscout_non_existent_pacman_dir_12345");
        let desktop_idx = DesktopIndex::new();
        let pacman_res = scan_pacman_packages(non_existent, None, &desktop_idx);
        assert!(pacman_res.is_empty());

        // 2. Flatpak provider points to corrupted directory with unreadable files
        let dir = tempdir().unwrap();
        let corrupt_flatpak = dir.path().join("flatpak_corrupt");
        fs::create_dir_all(&corrupt_flatpak).unwrap();
        fs::write(corrupt_flatpak.join("app"), b"not a directory").unwrap();
        let flatpak_res =
            scan_flatpak_installations(Some(&corrupt_flatpak), Some(&corrupt_flatpak));
        assert!(flatpak_res.is_empty());

        // 3. Steam provider given a directory with malformed libraryfolders.vdf
        let corrupt_steam = dir.path().join("steam_corrupt");
        fs::create_dir_all(corrupt_steam.join("steamapps")).unwrap();
        fs::write(
            corrupt_steam.join("steamapps/libraryfolders.vdf"),
            b"{{{corrupted data...",
        )
        .unwrap();
        let steam_res = scan_steam_installations(Some(&corrupt_steam));
        // Client entry is created, corrupted vdf does not panic
        assert_eq!(steam_res.len(), 1);
        assert_eq!(steam_res[0].id, "steam:client");
    }
}
