// SPDX-License-Identifier: GPL-3.0-or-later
//! User preferences stored in `$XDG_CONFIG_HOME/tuck/settings.json`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ShortcutChoice {
    SuperV,
    CtrlAltV,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct Settings {
    pub max_items: usize,
    pub auto_clear_days: Option<u32>,
    pub exclusions: Vec<String>,
    pub paused: bool,
    pub shortcut: ShortcutChoice,
    pub start_at_login: bool,
    pub show_tray_icon: bool,
    pub first_run_done: bool,
    pub saved_gnome_message_tray: Option<Vec<String>>,
    pub recent_emoji: Vec<String>,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            max_items: 50,
            auto_clear_days: None,
            exclusions: Vec::new(),
            paused: false,
            shortcut: ShortcutChoice::None,
            start_at_login: true,
            show_tray_icon: true,
            first_run_done: false,
            saved_gnome_message_tray: None,
            recent_emoji: Vec::new(),
        }
    }
}

impl Settings {
    /// A missing or unreadable file yields the defaults; settings never block startup.
    pub fn load(path: &Path) -> Settings {
        Self::load_checked(path).0
    }

    /// Like `load`, and also returns where a file that failed to parse was moved,
    /// so the next save does not overwrite it.
    pub fn load_checked(path: &Path) -> (Settings, Option<PathBuf>) {
        match fs::read(path) {
            Ok(bytes) => match serde_json::from_slice(&bytes) {
                Ok(settings) => (settings, None),
                Err(e) => {
                    log::warn!(
                        "settings file is not valid ({:?} at line {}, column {}), using defaults",
                        e.classify(),
                        e.line(),
                        e.column()
                    );
                    (Settings::default(), set_aside(path))
                }
            },
            Err(_) => (Settings::default(), None),
        }
    }

    /// Atomic write: a crash leaves either the old or the new file.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            crate::private::create_dir(parent)?;
        }
        let json = serde_json::to_vec_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let tmp = path.with_extension("json.tmp");
        crate::private::write_file(&tmp, &json)?;
        fs::rename(&tmp, path)
    }
}

/// Renames an unreadable settings file to `settings.corrupt-<unix>.json` and
/// drops older copies, so the user's values are not lost but files do not pile up.
fn set_aside(path: &Path) -> Option<PathBuf> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let backup = path.with_file_name(format!("settings.corrupt-{secs}.json"));
    if let Err(e) = fs::rename(path, &backup) {
        log::error!("cannot back up corrupt settings: {e}");
        return None;
    }
    if let Ok(entries) = fs::read_dir(backup.parent()?) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with("settings.corrupt-") && entry.path() != backup {
                let _ = fs::remove_file(entry.path());
            }
        }
    }
    Some(backup)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn missing_file_gives_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let s = Settings::load(&tmp.path().join("settings.json"));
        assert_eq!(s.max_items, 50);
        assert!(s.start_at_login);
        assert!(!s.first_run_done);
        assert_eq!(s.auto_clear_days, None);
    }

    #[test]
    fn corrupt_file_gives_defaults() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        fs::write(&path, b"{oops").unwrap();
        assert_eq!(
            Settings::load(&path),
            Settings::load(&tmp.path().join("none"))
        );
        assert_eq!(Settings::load(&path).max_items, 50);
    }

    #[test]
    fn round_trip_preserves_every_field() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("nested").join("settings.json");
        let s = Settings {
            max_items: 200,
            auto_clear_days: Some(7),
            exclusions: vec!["^sk-".into(), "secret".into()],
            paused: true,
            shortcut: ShortcutChoice::CtrlAltV,
            start_at_login: false,
            show_tray_icon: false,
            first_run_done: true,
            saved_gnome_message_tray: Some(vec!["<Super>v".into(), "<Super>m".into()]),
            recent_emoji: vec!["😀".into(), "🎉".into()],
        };
        s.save(&path).unwrap();
        assert_eq!(Settings::load(&path), s);
    }

    #[test]
    fn unknown_keys_are_ignored() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        fs::write(&path, br#"{"max_items": 75, "from_the_future": [1, 2]}"#).unwrap();
        let s = Settings::load(&path);
        assert_eq!(s.max_items, 75);
        assert!(s.start_at_login);
        // A file written before the icon existed shows it.
        assert!(s.show_tray_icon);
    }

    #[test]
    fn corrupt_file_is_set_aside_not_overwritten() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        fs::write(&path, b"{oops").unwrap();
        let (s, backup) = Settings::load_checked(&path);
        assert_eq!(s, Settings::default());
        let backup = backup.expect("backup path");
        assert!(backup
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("settings.corrupt-"));
        assert_eq!(fs::read(&backup).unwrap(), b"{oops");
        assert!(!path.exists());
    }

    #[test]
    fn only_newest_corrupt_copy_is_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("settings.json");
        let stale = tmp.path().join("settings.corrupt-1.json");
        fs::write(&stale, b"old").unwrap();
        fs::write(&path, b"{oops").unwrap();
        let (_, backup) = Settings::load_checked(&path);
        assert!(backup.unwrap().exists());
        assert!(!stale.exists());
    }

    #[test]
    fn saved_settings_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("cfg");
        let path = dir.join("settings.json");
        Settings::default().save(&path).unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&path), 0o600);
        // An older world-readable file is tightened by the next save.
        fs::set_permissions(&path, fs::Permissions::from_mode(0o664)).unwrap();
        Settings::default().save(&path).unwrap();
        assert_eq!(mode(&path), 0o600);
    }
}
