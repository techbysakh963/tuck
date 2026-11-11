//! Terminal detection on X11 from the active window's WM_CLASS.

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt as _};
use x11rb::rust_connection::RustConnection;

use super::TargetKind;

const TERMINALS: &[&str] = &[
    "gnome-terminal-server",
    "gnome-terminal",
    "ptyxis",
    "kgx",
    "konsole",
    "xterm",
    "urxvt",
    "alacritty",
    "kitty",
    "wezterm",
    "foot",
    "tilix",
    "terminator",
    "xfce4-terminal",
    "mate-terminal",
    "qterminal",
    "lxterminal",
    "st",
    "ghostty",
    "warp",
];

/// True when one of the class strings is a known terminal. Accepts the quoted
/// form printed by `xprop` (`"instance", "Class"`) or the raw NUL-separated
/// property value, and ignores the `WM_CLASS(STRING) =` header. Tokens must
/// match whole, so "firefox" never matches "st".
pub fn wm_class_is_terminal(class_value: &str) -> bool {
    let tokens: Vec<&str> = if class_value.contains('"') {
        class_value
            .split('"')
            .enumerate()
            .filter(|(i, _)| i % 2 == 1)
            .map(|(_, t)| t)
            .collect()
    } else {
        class_value.split(['\0', ',']).map(str::trim).collect()
    };
    tokens.into_iter().any(|token| {
        let token = token.to_ascii_lowercase();
        // Reverse-DNS class names such as "org.gnome.Ptyxis" end in the name.
        let last = token.rsplit('.').next().unwrap_or(&token);
        TERMINALS.contains(&token.as_str()) || TERMINALS.contains(&last)
    })
}

pub fn detect() -> TargetKind {
    match active_wm_class() {
        Some(class) if wm_class_is_terminal(&class) => TargetKind::Terminal,
        Some(_) => TargetKind::Other,
        None => TargetKind::Unknown,
    }
}

fn active_wm_class() -> Option<String> {
    let (conn, screen) = RustConnection::connect(None).ok()?;
    let root = conn.setup().roots[screen].root;
    let intern = |name: &[u8]| {
        conn.intern_atom(false, name)
            .ok()?
            .reply()
            .ok()
            .map(|r| r.atom)
    };
    let active = intern(b"_NET_ACTIVE_WINDOW")?;
    let reply = conn
        .get_property(false, root, active, AtomEnum::WINDOW, 0, 1)
        .ok()?
        .reply()
        .ok()?;
    let window = reply.value32()?.next().filter(|&w| w != 0)?;
    let class = conn
        .get_property(false, window, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256)
        .ok()?
        .reply()
        .ok()?;
    if class.value.is_empty() {
        return None;
    }
    // Keep the NUL separators; the parser handles them.
    Some(String::from_utf8_lossy(&class.value).into_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_classes_match() {
        assert!(wm_class_is_terminal(
            r#""gnome-terminal-server", "Gnome-terminal""#
        ));
        assert!(wm_class_is_terminal("kitty\0kitty\0"));
        assert!(wm_class_is_terminal(r#""org.gnome.Ptyxis", "Ptyxis""#));
    }

    #[test]
    fn other_classes_do_not_match() {
        assert!(!wm_class_is_terminal(r#""code", "Code""#));
        // The raw xprop header used to match "st" as a substring of "STRING".
        assert!(!wm_class_is_terminal(
            r#"WM_CLASS(STRING) = "firefox", "firefox""#
        ));
        assert!(!wm_class_is_terminal("firefox\0firefox\0"));
        assert!(!wm_class_is_terminal(""));
    }
}
