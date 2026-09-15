//! XDG user directories (M4).
//!
//! Resolves the well-known user locations (`Documents`, `Downloads`, …)
//! so the classifier can grant subtree ownership instead of guessing from
//! file extensions. Precedence per directory: `$XDG_*_DIR` environment
//! (absolute paths only), then `~/.config/user-dirs.dirs`, then
//! `$HOME/<Default>`. Relative entries resolve against `$HOME`.
//!
//! Only parsing, no I/O beyond one small config file; unreadable or
//! malformed input falls back to defaults without failing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Resolved absolute user directories. Only the categories DiskScout
/// displays need entries; `Desktop` maps to Other (documented in
/// `classify.rs`).
#[derive(Debug, Clone)]
pub struct XdgDirs {
    pub documents: PathBuf,
    pub downloads: PathBuf,
    pub music: PathBuf,
    pub pictures: PathBuf,
    pub videos: PathBuf,
}

impl XdgDirs {
    /// Resolve against `home`. Never fails: every key falls back to
    /// `$HOME/<Default>`.
    pub fn resolve(home: &Path) -> Self {
        let file = parse_user_dirs_dotfile(&home.join(".config/user-dirs.dirs"));
        let pick = |env_key: &str, file_key: &str, default: &str| -> PathBuf {
            if let Some(path) = std::env::var_os(env_key)
                .map(PathBuf::from)
                .filter(|p| p.is_absolute())
            {
                return path;
            }
            if let Some(path) = file.get(file_key) {
                return path.clone();
            }
            home.join(default)
        };
        Self {
            documents: pick("XDG_DOCUMENTS_DIR", "XDG_DOCUMENTS_DIR", "Documents"),
            downloads: pick("XDG_DOWNLOAD_DIR", "XDG_DOWNLOAD_DIR", "Downloads"),
            music: pick("XDG_MUSIC_DIR", "XDG_MUSIC_DIR", "Music"),
            pictures: pick("XDG_PICTURES_DIR", "XDG_PICTURES_DIR", "Pictures"),
            videos: pick("XDG_VIDEOS_DIR", "XDG_VIDEOS_DIR", "Videos"),
        }
    }
}

/// Parse `user-dirs.dirs` into absolute paths. Accepted line forms:
///
/// ```text
/// XDG_DOWNLOAD_DIR="$HOME/Downloads"
/// XDG_DOCUMENTS_DIR=/mnt/data/docs
/// XDG_MUSIC_DIR=Music
/// ```
///
/// Comments (`#…`), blank lines and malformed entries are ignored.
/// `$HOME`/`${HOME}` prefixes and bare relative paths resolve against
/// `home`; anything else must already be absolute.
fn parse_user_dirs_dotfile(path: &Path) -> HashMap<String, PathBuf> {
    let home = path
        .parent()
        .and_then(|p| p.parent())
        .unwrap_or(Path::new("/"));
    let text = std::fs::read_to_string(path).unwrap_or_default();
    let mut out = HashMap::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') || !line.starts_with("XDG_") {
            continue;
        }
        let Some((key, mut value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        value = value.trim().trim_matches('"').trim();
        if key.is_empty() || value.is_empty() {
            continue;
        }
        let resolved = if let Some(rest) = value
            .strip_prefix("$HOME/")
            .or_else(|| value.strip_prefix("${HOME}/"))
        {
            home.join(rest)
        } else if value == "$HOME" || value == "${HOME}" {
            home.to_path_buf()
        } else {
            PathBuf::from(value)
        };
        let resolved = if resolved.is_absolute() {
            resolved
        } else {
            home.join(resolved)
        };
        out.insert(key.to_string(), resolved);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex, OnceLock};

    /// Environment mutation must not race other tests.
    fn env_lock() -> &'static Mutex<()> {
        static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
        LOCK.get_or_init(|| Mutex::new(()))
    }

    const KEYS: [&str; 5] = [
        "XDG_DOCUMENTS_DIR",
        "XDG_DOWNLOAD_DIR",
        "XDG_MUSIC_DIR",
        "XDG_PICTURES_DIR",
        "XDG_VIDEOS_DIR",
    ];

    fn clear_env() {
        for key in KEYS {
            unsafe { std::env::remove_var(key) };
        }
    }

    #[test]
    fn defaults_without_config_or_env() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        let home = Path::new("/home/tester");
        let dirs = XdgDirs::resolve(home);
        assert_eq!(dirs.documents, PathBuf::from("/home/tester/Documents"));
        assert_eq!(dirs.downloads, PathBuf::from("/home/tester/Downloads"));
        assert_eq!(dirs.music, PathBuf::from("/home/tester/Music"));
        assert_eq!(dirs.pictures, PathBuf::from("/home/tester/Pictures"));
        assert_eq!(dirs.videos, PathBuf::from("/home/tester/Videos"));
    }

    #[test]
    fn parses_dotfile_forms() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::create_dir_all(home.join(".config")).unwrap();
        std::fs::write(
            home.join(".config/user-dirs.dirs"),
            "# comment\n\nXDG_DOCUMENTS_DIR=\"$HOME/Docs\"\nXDG_DOWNLOAD_DIR=/mnt/dl\nXDG_MUSIC_DIR=Tunes\nmalformed line\nXDG_VIDEOS_DIR=\nXDG_PICTURES_DIR=\"${HOME}/Shots\"\n",
        )
        .unwrap();

        let parsed = parse_user_dirs_dotfile(&home.join(".config/user-dirs.dirs"));
        assert_eq!(parsed["XDG_DOCUMENTS_DIR"], home.join("Docs"));
        assert_eq!(parsed["XDG_DOWNLOAD_DIR"], PathBuf::from("/mnt/dl"));
        assert_eq!(parsed["XDG_MUSIC_DIR"], home.join("Tunes"));
        assert_eq!(parsed["XDG_PICTURES_DIR"], home.join("Shots"));
        assert!(!parsed.contains_key("XDG_VIDEOS_DIR"));
        assert!(!parsed.contains_key("malformed line"));
    }

    #[test]
    fn env_beats_file_beats_default() {
        let _guard = env_lock().lock().unwrap();
        clear_env();
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        std::fs::create_dir_all(home.join(".config")).unwrap();
        std::fs::write(
            home.join(".config/user-dirs.dirs"),
            "XDG_DOWNLOAD_DIR=\"$HOME/FromFile\"\n",
        )
        .unwrap();

        // File wins over the default.
        assert_eq!(XdgDirs::resolve(home).downloads, home.join("FromFile"));
        // Env wins over the file (absolute only).
        unsafe { std::env::set_var("XDG_DOWNLOAD_DIR", "/mnt/env-dl") };
        assert_eq!(
            XdgDirs::resolve(home).downloads,
            PathBuf::from("/mnt/env-dl")
        );
        // Relative env values are ignored, file applies again.
        unsafe { std::env::set_var("XDG_DOWNLOAD_DIR", "relative/ignored") };
        assert_eq!(XdgDirs::resolve(home).downloads, home.join("FromFile"));
        clear_env();
    }

    #[test]
    fn missing_file_resolves_defaults() {
        let parsed = parse_user_dirs_dotfile(Path::new("/nonexistent/user-dirs.dirs"));
        assert!(parsed.is_empty());
    }
}
