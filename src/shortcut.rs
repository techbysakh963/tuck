// SPDX-License-Identifier: GPL-3.0-or-later
//! Registering Tuck's shortcut with GNOME, and undoing it.

use gtk4::gio;
use gtk4::prelude::*;

use crate::settings::{Settings, ShortcutChoice};

const MEDIA_KEYS: &str = "org.gnome.settings-daemon.plugins.media-keys";
const CUSTOM_SCHEMA: &str = "org.gnome.settings-daemon.plugins.media-keys.custom-keybinding";
const SHELL_KEYBINDINGS: &str = "org.gnome.shell.keybindings";
const LIST_KEY: &str = "custom-keybindings";
const TRAY_KEY: &str = "toggle-message-tray";
pub const TUCK_PATH: &str =
    "/org/gnome/settings-daemon/plugins/media-keys/custom-keybindings/tuck/";
const SUPER_V: &str = "<Super>v";

/// `list` with `path` added, once, at the end.
pub fn with_path(list: &[String], path: &str) -> Vec<String> {
    let mut out = list.to_vec();
    if !out.iter().any(|p| p == path) {
        out.push(path.to_string());
    }
    out
}

/// `list` without any occurrence of `path`.
pub fn without_path(list: &[String], path: &str) -> Vec<String> {
    list.iter().filter(|p| *p != path).cloned().collect()
}

/// The accelerator string GNOME expects for a choice.
pub fn binding_for(choice: ShortcutChoice) -> Option<&'static str> {
    match choice {
        ShortcutChoice::SuperV => Some("<Super>v"),
        ShortcutChoice::CtrlAltV => Some("<Ctrl><Alt>v"),
        ShortcutChoice::None => None,
    }
}

/// Human wording for the Preferences page.
pub fn describe(choice: ShortcutChoice) -> &'static str {
    match choice {
        ShortcutChoice::SuperV => "Super+V",
        ShortcutChoice::CtrlAltV => "Ctrl+Alt+V",
        ShortcutChoice::None => "Not set",
    }
}

fn schema_available(id: &str) -> bool {
    gio::SettingsSchemaSource::default().is_some_and(|source| source.lookup(id, true).is_some())
}

/// True on a GNOME session whose settings daemon schemas are installed.
pub fn gnome_supported() -> bool {
    let desktop = std::env::var("XDG_CURRENT_DESKTOP").unwrap_or_default();
    desktop.split(':').any(|d| d.eq_ignore_ascii_case("GNOME"))
        && schema_available(MEDIA_KEYS)
        && schema_available(CUSTOM_SCHEMA)
        && schema_available(SHELL_KEYBINDINGS)
}

fn flush() {
    gio::Settings::sync();
}

/// Gives GNOME's notification-list shortcut back its original value.
fn restore_message_tray(settings: &mut Settings) {
    if let Some(saved) = settings.saved_gnome_message_tray.take() {
        let tray = gio::Settings::new(SHELL_KEYBINDINGS);
        let refs: Vec<&str> = saved.iter().map(String::as_str).collect();
        if let Err(e) = tray.set_strv(TRAY_KEY, refs.as_slice()) {
            log::warn!("cannot restore {TRAY_KEY}: {e}");
        }
    }
}

/// Registers `tuck --toggle` under the chosen key. For Super+V the key is first
/// taken away from the notification list, with the original value remembered
/// in `settings` so it can be restored.
pub fn install_gnome(choice: ShortcutChoice, settings: &mut Settings) -> Result<(), String> {
    let Some(binding) = binding_for(choice) else {
        reset_gnome(settings);
        return Ok(());
    };
    if !gnome_supported() {
        return Err("manual".to_string());
    }

    // Moving between choices must not lose the original notification binding.
    restore_message_tray(settings);

    let keys = gio::Settings::new(MEDIA_KEYS);
    let current: Vec<String> = keys.strv(LIST_KEY).iter().map(|s| s.to_string()).collect();
    let updated = with_path(&current, TUCK_PATH);
    let refs: Vec<&str> = updated.iter().map(String::as_str).collect();
    keys.set_strv(LIST_KEY, refs.as_slice())
        .map_err(|e| e.to_string())?;

    let custom = gio::Settings::with_path(CUSTOM_SCHEMA, TUCK_PATH);
    custom
        .set_string("name", "Tuck")
        .map_err(|e| e.to_string())?;
    custom
        .set_string("command", "tuck --toggle")
        .map_err(|e| e.to_string())?;
    custom
        .set_string("binding", binding)
        .map_err(|e| e.to_string())?;

    if choice == ShortcutChoice::SuperV {
        let tray = gio::Settings::new(SHELL_KEYBINDINGS);
        let original: Vec<String> = tray.strv(TRAY_KEY).iter().map(|s| s.to_string()).collect();
        if original.iter().any(|b| b == SUPER_V) {
            let kept: Vec<&str> = original
                .iter()
                .map(String::as_str)
                .filter(|b| *b != SUPER_V)
                .collect();
            settings.saved_gnome_message_tray = Some(original.clone());
            tray.set_strv(TRAY_KEY, kept.as_slice())
                .map_err(|e| e.to_string())?;
        }
    }

    settings.shortcut = choice;
    flush();
    Ok(())
}

/// Removes Tuck's binding and restores the notification-list shortcut.
pub fn reset_gnome(settings: &mut Settings) {
    if gnome_supported() {
        let keys = gio::Settings::new(MEDIA_KEYS);
        let current: Vec<String> = keys.strv(LIST_KEY).iter().map(|s| s.to_string()).collect();
        let updated = without_path(&current, TUCK_PATH);
        let refs: Vec<&str> = updated.iter().map(String::as_str).collect();
        if let Err(e) = keys.set_strv(LIST_KEY, refs.as_slice()) {
            log::warn!("cannot remove the Tuck keybinding: {e}");
        }
        let custom = gio::Settings::with_path(CUSTOM_SCHEMA, TUCK_PATH);
        for key in ["name", "command", "binding"] {
            custom.reset(key);
        }
        restore_message_tray(settings);
        flush();
    }
    settings.shortcut = ShortcutChoice::None;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn with_path_is_idempotent_and_keeps_order() {
        let once = with_path(&v(&["/a/", "/b/"]), "/tuck/");
        assert_eq!(once, v(&["/a/", "/b/", "/tuck/"]));
        assert_eq!(with_path(&once, "/tuck/"), once);
    }

    #[test]
    fn with_path_handles_an_empty_list() {
        assert_eq!(with_path(&[], "/tuck/"), v(&["/tuck/"]));
    }

    #[test]
    fn without_path_removes_only_ours() {
        let list = v(&["/a/", "/tuck/", "/b/"]);
        assert_eq!(without_path(&list, "/tuck/"), v(&["/a/", "/b/"]));
        assert_eq!(
            without_path(&v(&["/tuck/"]), "/tuck/"),
            Vec::<String>::new()
        );
        assert_eq!(without_path(&[], "/tuck/"), Vec::<String>::new());
        assert_eq!(without_path(&list, "/none/"), list);
    }

    #[test]
    fn bindings_match_the_choices() {
        assert_eq!(binding_for(ShortcutChoice::SuperV), Some("<Super>v"));
        assert_eq!(binding_for(ShortcutChoice::CtrlAltV), Some("<Ctrl><Alt>v"));
        assert_eq!(binding_for(ShortcutChoice::None), None);
    }

    /// Run with a throwaway settings backend so the real desktop is untouched:
    /// `GSETTINGS_BACKEND=keyfile XDG_CONFIG_HOME=$(mktemp -d) XDG_CURRENT_DESKTOP=GNOME
    /// cargo test live_install_and_reset -- --ignored`
    #[test]
    #[ignore = "needs GNOME schemas and a keyfile settings backend"]
    fn live_install_and_reset_round_trip() {
        assert_eq!(
            std::env::var("GSETTINGS_BACKEND").as_deref(),
            Ok("keyfile"),
            "refusing to touch the real desktop settings"
        );
        let tray = gio::Settings::new(SHELL_KEYBINDINGS);
        let original: Vec<String> = tray.strv(TRAY_KEY).iter().map(|s| s.to_string()).collect();
        assert!(original.iter().any(|b| b == SUPER_V));

        let mut settings = Settings::default();
        install_gnome(ShortcutChoice::SuperV, &mut settings).unwrap();
        let now: Vec<String> = tray.strv(TRAY_KEY).iter().map(|s| s.to_string()).collect();
        assert!(!now.iter().any(|b| b == SUPER_V));
        let keys = gio::Settings::new(MEDIA_KEYS);
        assert!(keys.strv(LIST_KEY).iter().any(|p| p == TUCK_PATH));
        let custom = gio::Settings::with_path(CUSTOM_SCHEMA, TUCK_PATH);
        assert_eq!(custom.string("binding"), "<Super>v");
        assert_eq!(custom.string("command"), "tuck --toggle");

        // Installing twice must not overwrite the remembered original.
        install_gnome(ShortcutChoice::SuperV, &mut settings).unwrap();
        install_gnome(ShortcutChoice::CtrlAltV, &mut settings).unwrap();
        let back: Vec<String> = tray.strv(TRAY_KEY).iter().map(|s| s.to_string()).collect();
        assert_eq!(back, original);
        assert_eq!(custom.string("binding"), "<Ctrl><Alt>v");
        assert_eq!(
            keys.strv(LIST_KEY)
                .iter()
                .filter(|p| *p == TUCK_PATH)
                .count(),
            1
        );

        install_gnome(ShortcutChoice::SuperV, &mut settings).unwrap();
        reset_gnome(&mut settings);
        let reset: Vec<String> = tray.strv(TRAY_KEY).iter().map(|s| s.to_string()).collect();
        assert_eq!(reset, original);
        assert!(!keys.strv(LIST_KEY).iter().any(|p| p == TUCK_PATH));
        assert_eq!(settings.shortcut, ShortcutChoice::None);
        assert_eq!(settings.saved_gnome_message_tray, None);
    }
}
