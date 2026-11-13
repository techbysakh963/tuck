// SPDX-License-Identifier: GPL-3.0-or-later
//! Start at login through an XDG autostart entry.

use std::fs;
use std::io;
use std::path::PathBuf;

use crate::APP_ID;

fn entry_path() -> PathBuf {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))
        .unwrap_or_else(|| PathBuf::from(".config"));
    base.join("autostart").join(format!("{APP_ID}.desktop"))
}

pub fn is_enabled() -> bool {
    entry_path().exists()
}

/// Writes or removes the autostart entry. Removing a missing entry is fine.
pub fn set_enabled(on: bool) -> io::Result<()> {
    let path = entry_path();
    if !on {
        return match fs::remove_file(&path) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        };
    }
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let entry = format!(
        "[Desktop Entry]\nType=Application\nName=Tuck\nComment=Clipboard history\n\
         Exec=tuck --background\nIcon={APP_ID}\nTerminal=false\n\
         X-GNOME-Autostart-enabled=true\n"
    );
    fs::write(path, entry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enable_then_disable_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        // Only this test touches the variable.
        std::env::set_var("XDG_CONFIG_HOME", tmp.path());
        assert!(!is_enabled());

        set_enabled(true).unwrap();
        let file = tmp
            .path()
            .join("autostart")
            .join(format!("{APP_ID}.desktop"));
        let text = std::fs::read_to_string(&file).unwrap();
        assert!(text.contains("Exec=tuck --background"));
        assert!(is_enabled());

        set_enabled(true).unwrap();
        set_enabled(false).unwrap();
        assert!(!file.exists());
        set_enabled(false).unwrap();
    }
}
