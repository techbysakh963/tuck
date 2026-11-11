//! Works out whether the window that will receive a paste is a terminal, which
//! decides between Ctrl+V and Ctrl+Shift+V.

pub mod a11y;
pub mod x11;

use std::sync::mpsc;
use std::time::Duration;

use crate::paste::Combo;

/// Upper bound for the whole detection.
const DETECT_CAP: Duration = Duration::from_millis(300);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetKind {
    Terminal,
    Other,
    Unknown,
}

/// Detects the focused target on its own thread so a slow accessibility bus
/// can never stall the caller; gives up after 300 ms. Must not be called from
/// inside a D-Bus method handler.
pub fn detect_target(is_x11: bool) -> TargetKind {
    let (tx, rx) = mpsc::channel();
    let spawned = std::thread::Builder::new()
        .name("tuck-detect".into())
        .spawn(move || {
            let kind = if is_x11 {
                x11::detect()
            } else {
                a11y::detect()
            };
            let _ = tx.send(kind);
        });
    if spawned.is_err() {
        return TargetKind::Unknown;
    }
    rx.recv_timeout(DETECT_CAP).unwrap_or(TargetKind::Unknown)
}

/// Terminals (or a forced choice) take Ctrl+Shift+V, everything else Ctrl+V.
pub fn combo_for(kind: TargetKind, force_terminal: bool) -> Combo {
    if force_terminal || kind == TargetKind::Terminal {
        Combo::CtrlShiftV
    } else {
        Combo::CtrlV
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combo_table() {
        use TargetKind::*;
        assert_eq!(combo_for(Terminal, false), Combo::CtrlShiftV);
        assert_eq!(combo_for(Other, false), Combo::CtrlV);
        assert_eq!(combo_for(Unknown, false), Combo::CtrlV);
        assert_eq!(combo_for(Terminal, true), Combo::CtrlShiftV);
        assert_eq!(combo_for(Other, true), Combo::CtrlShiftV);
        assert_eq!(combo_for(Unknown, true), Combo::CtrlShiftV);
    }

    #[test]
    #[ignore = "needs a desktop session with a focused window"]
    fn live_detect_probe() {
        let is_x11 = std::env::var_os("WAYLAND_DISPLAY").is_none();
        let start = std::time::Instant::now();
        let kind = detect_target(is_x11);
        println!("{kind:?} in {:?}", start.elapsed());
    }
}
