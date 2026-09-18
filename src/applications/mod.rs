pub mod desktop;
pub mod flatpak;
pub mod icon;
pub mod linux;
pub mod model;
pub mod pacman;
pub mod steam;

pub use icon::{IconResolver, UiImageCache};
pub use model::{Application, ApplicationKind, ApplicationSource, InstallScope};

use std::sync::{Arc, Mutex};

/// Shared session cache for discovered installed applications.
pub type ApplicationStore = Arc<Mutex<Option<Vec<Application>>>>;

/// Discover installed applications for the current platform.
pub fn discover_applications() -> Vec<Application> {
    #[cfg(target_os = "linux")]
    {
        linux::discover_linux_applications()
    }

    #[cfg(not(target_os = "linux"))]
    {
        Vec::new()
    }
}

#[cfg(test)]
mod live_tests {

    use super::*;

    #[test]
    fn test_live_discovery_smoke() {
        let apps = discover_applications();
        println!("Live discovered {} applications", apps.len());
        // Verify at least some applications were discovered if running on Linux
        #[cfg(target_os = "linux")]
        {
            if std::path::Path::new("/var/lib/pacman/local").is_dir() {
                assert!(
                    !apps.is_empty(),
                    "Pacman db exists so applications should be found"
                );
            }
        }
    }
}
