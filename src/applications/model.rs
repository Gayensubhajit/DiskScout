use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ApplicationSource {
    Pacman,
    Foreign,
    Flatpak,
    Steam,
    AppImage,
    Manual,
}

impl ApplicationSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pacman => "Pacman",
            Self::Foreign => "Foreign",
            Self::Flatpak => "Flatpak",
            Self::Steam => "Steam",
            Self::AppImage => "AppImage",
            Self::Manual => "Manual",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum InstallScope {
    System,
    User,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub enum ApplicationKind {
    Application,
    Runtime,
    Game,
    Tool,
    Library,
}

impl ApplicationKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            ApplicationKind::Application => "Application",
            ApplicationKind::Runtime => "Runtime",
            ApplicationKind::Game => "Game",
            ApplicationKind::Tool => "Tool",
            ApplicationKind::Library => "Library",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Application {
    pub id: String,
    pub name: String,
    pub display_name: String,
    pub version: String,
    pub description: String,
    pub source: ApplicationSource,
    pub kind: ApplicationKind,
    pub install_scope: InstallScope,
    pub package_size: u64,
    pub measured_size: Option<u64>,
    pub user_data_size: Option<u64>,
    pub runtime_size: Option<u64>,
    pub location: Option<PathBuf>,
    pub desktop_file: Option<PathBuf>,
    pub icon_name: String,
    pub is_explicit: bool,
}

impl Application {
    /// Return the primary size metric to display.
    pub fn display_size(&self) -> u64 {
        self.measured_size.unwrap_or(self.package_size)
    }

    pub fn is_game(&self) -> bool {
        self.kind == ApplicationKind::Game
    }

    pub fn is_runtime(&self) -> bool {
        self.kind == ApplicationKind::Runtime
            || self.kind == ApplicationKind::Library
            || (self.kind == ApplicationKind::Tool
                && (self.display_name.contains("Runtime")
                    || self.display_name.contains("Proton")
                    || self.description.contains("Runtime")))
    }

    pub fn is_app(&self) -> bool {
        !self.is_game() && !self.is_runtime()
    }

    pub fn matches_filter(&self, filter: &str) -> bool {
        match filter {
            "apps" => self.is_app(),
            "games" => self.is_game(),
            "runtimes" => self.is_runtime(),
            _ => true, // "all"
        }
    }

    /// Case-insensitive search match against display name, package name, description, and source.
    pub fn matches_search(&self, query: &str) -> bool {
        let q = query.trim().to_lowercase();
        if q.is_empty() {
            return true;
        }
        self.display_name.to_lowercase().contains(&q)
            || self.name.to_lowercase().contains(&q)
            || self.description.to_lowercase().contains(&q)
            || self.source.as_str().to_lowercase().contains(&q)
    }
}

/// Sort applications according to user preference.
pub fn sort_applications(apps: &mut [Application], sort_key: &str) {
    match sort_key {
        "size_asc" => apps.sort_by(|a, b| {
            a.display_size()
                .cmp(&b.display_size())
                .then_with(|| a.display_name.cmp(&b.display_name))
        }),
        "name_asc" => apps.sort_by(|a, b| {
            a.display_name
                .to_lowercase()
                .cmp(&b.display_name.to_lowercase())
                .then_with(|| b.display_size().cmp(&a.display_size()))
        }),
        "name_desc" => apps.sort_by(|a, b| {
            b.display_name
                .to_lowercase()
                .cmp(&a.display_name.to_lowercase())
                .then_with(|| b.display_size().cmp(&a.display_size()))
        }),
        "source" => apps.sort_by(|a, b| {
            a.source
                .as_str()
                .cmp(b.source.as_str())
                .then_with(|| {
                    a.display_name
                        .to_lowercase()
                        .cmp(&b.display_name.to_lowercase())
                })
                .then_with(|| b.display_size().cmp(&a.display_size()))
        }),
        "type" => apps.sort_by(|a, b| {
            a.kind
                .as_str()
                .cmp(b.kind.as_str())
                .then_with(|| {
                    a.display_name
                        .to_lowercase()
                        .cmp(&b.display_name.to_lowercase())
                })
                .then_with(|| b.display_size().cmp(&a.display_size()))
        }),
        _ => apps.sort_by(|a, b| {
            b.display_size()
                .cmp(&a.display_size())
                .then_with(|| a.display_name.cmp(&b.display_name))
        }), // default size_desc
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_application_source_label() {
        assert_eq!(ApplicationSource::Pacman.as_str(), "Pacman");
        assert_eq!(ApplicationSource::Foreign.as_str(), "Foreign");
        assert_eq!(ApplicationSource::Flatpak.as_str(), "Flatpak");
        assert_eq!(ApplicationSource::Steam.as_str(), "Steam");
    }

    #[test]
    fn test_sort_applications() {
        let mut apps = vec![
            Application {
                id: "1".into(),
                name: "b".into(),
                display_name: "Beta".into(),
                version: "1.0".into(),
                description: "".into(),
                source: ApplicationSource::Pacman,
                kind: ApplicationKind::Application,
                install_scope: InstallScope::System,
                package_size: 100,
                measured_size: None,
                user_data_size: None,
                runtime_size: None,
                location: None,
                desktop_file: None,
                icon_name: "package".into(),
                is_explicit: true,
            },
            Application {
                id: "2".into(),
                name: "a".into(),
                display_name: "Alpha".into(),
                version: "2.0".into(),
                description: "".into(),
                source: ApplicationSource::Foreign,
                kind: ApplicationKind::Application,
                install_scope: InstallScope::System,
                package_size: 200,
                measured_size: None,
                user_data_size: None,
                runtime_size: None,
                location: None,
                desktop_file: None,
                icon_name: "package".into(),
                is_explicit: true,
            },
        ];

        // Default size_desc
        sort_applications(&mut apps, "size_desc");
        assert_eq!(apps[0].display_name, "Alpha");
        assert_eq!(apps[1].display_name, "Beta");

        // size_asc
        sort_applications(&mut apps, "size_asc");
        assert_eq!(apps[0].display_name, "Beta");
        assert_eq!(apps[1].display_name, "Alpha");

        // name_asc
        sort_applications(&mut apps, "name_asc");
        assert_eq!(apps[0].display_name, "Alpha");
        assert_eq!(apps[1].display_name, "Beta");
    }
    #[test]
    fn test_application_filters_and_counts() {
        let app = Application {
            id: "1".into(),
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

        let game = Application {
            id: "2".into(),
            name: "wallpaper_engine".into(),
            display_name: "Wallpaper Engine".into(),
            version: "1.0".into(),
            description: "Steam Game".into(),
            source: ApplicationSource::Steam,
            kind: ApplicationKind::Game,
            install_scope: InstallScope::User,
            package_size: 800_000_000,
            measured_size: Some(800_000_000),
            user_data_size: None,
            runtime_size: None,
            location: None,
            desktop_file: None,
            icon_name: "steam".into(),
            is_explicit: true,
        };

        let runtime = Application {
            id: "3".into(),
            name: "proton".into(),
            display_name: "Proton Experimental".into(),
            version: "1.0".into(),
            description: "Steam Tool / Runtime".into(),
            source: ApplicationSource::Steam,
            kind: ApplicationKind::Tool,
            install_scope: InstallScope::User,
            package_size: 1_400_000_000,
            measured_size: Some(1_400_000_000),
            user_data_size: None,
            runtime_size: None,
            location: None,
            desktop_file: None,
            icon_name: "steam".into(),
            is_explicit: true,
        };

        let apps = vec![app, game, runtime];

        let count_all = apps.iter().filter(|a| a.matches_filter("all")).count();
        let count_apps = apps.iter().filter(|a| a.matches_filter("apps")).count();
        let count_games = apps.iter().filter(|a| a.matches_filter("games")).count();
        let count_runtimes = apps.iter().filter(|a| a.matches_filter("runtimes")).count();

        assert_eq!(count_all, 3);
        assert_eq!(count_apps, 1);
        assert_eq!(count_games, 1);
        assert_eq!(count_runtimes, 1);
    }
    #[test]
    fn test_application_search() {
        let app = Application {
            id: "1".into(),
            name: "org.videolan.vlc".into(),
            display_name: "VLC media player".into(),
            version: "3.0.18".into(),
            description: "Multimedia player and framework".into(),
            source: ApplicationSource::Flatpak,
            kind: ApplicationKind::Application,
            install_scope: InstallScope::User,
            package_size: 50_000_000,
            measured_size: None,
            user_data_size: None,
            runtime_size: None,
            location: None,
            desktop_file: None,
            icon_name: "vlc".into(),
            is_explicit: true,
        };

        // Match display name
        assert!(app.matches_search("vlc"));
        assert!(app.matches_search("VLC"));
        assert!(app.matches_search("player"));

        // Match package identifier
        assert!(app.matches_search("videolan"));

        // Match description
        assert!(app.matches_search("multimedia"));

        // Match source
        assert!(app.matches_search("flatpak"));

        // Non-match
        assert!(!app.matches_search("firefox"));
        // Empty query matches all
        assert!(app.matches_search(""));
        assert!(app.matches_search("   "));
    }

    #[test]
    fn test_sort_applications_all_keys() {
        let mut apps = vec![
            Application {
                id: "1".into(),
                name: "pkg-z".into(),
                display_name: "Zeta".into(),
                version: "1.0".into(),
                description: "".into(),
                source: ApplicationSource::Pacman,
                kind: ApplicationKind::Game,
                install_scope: InstallScope::System,
                package_size: 100,
                measured_size: None,
                user_data_size: None,
                runtime_size: None,
                location: None,
                desktop_file: None,
                icon_name: "pkg".into(),
                is_explicit: true,
            },
            Application {
                id: "2".into(),
                name: "pkg-a".into(),
                display_name: "Alpha".into(),
                version: "2.0".into(),
                description: "".into(),
                source: ApplicationSource::Flatpak,
                kind: ApplicationKind::Application,
                install_scope: InstallScope::User,
                package_size: 500,
                measured_size: None,
                user_data_size: None,
                runtime_size: None,
                location: None,
                desktop_file: None,
                icon_name: "pkg".into(),
                is_explicit: true,
            },
        ];

        // name_desc
        sort_applications(&mut apps, "name_desc");
        assert_eq!(apps[0].display_name, "Zeta");
        assert_eq!(apps[1].display_name, "Alpha");

        // source
        sort_applications(&mut apps, "source");
        assert_eq!(apps[0].source, ApplicationSource::Flatpak);
        assert_eq!(apps[1].source, ApplicationSource::Pacman);

        // type
        sort_applications(&mut apps, "type");
        assert_eq!(apps[0].kind, ApplicationKind::Application);
        assert_eq!(apps[1].kind, ApplicationKind::Game);
    }
}
