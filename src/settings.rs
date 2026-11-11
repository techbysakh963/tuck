//! User preferences stored in `$XDG_CONFIG_HOME/tuck/settings.json`.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

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
            first_run_done: false,
            saved_gnome_message_tray: None,
            recent_emoji: Vec::new(),
        }
    }
}

impl Settings {
    /// A missing or unreadable file yields the defaults; settings never block startup.
    pub fn load(path: &Path) -> Settings {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                log::warn!("settings file is not valid, using defaults: {e}");
                Settings::default()
            }),
            Err(_) => Settings::default(),
        }
    }

    /// Atomic write: a crash leaves either the old or the new file.
    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let json = serde_json::to_vec_pretty(self)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;
        let tmp = path.with_extension("json.tmp");
        {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(&json)?;
            f.sync_all()?;
        }
        fs::rename(&tmp, path)
    }
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
    }
}
