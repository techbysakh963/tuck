//! Paste keystrokes: the key combination and the senders that deliver it.

pub mod uinput;
pub mod xtest;

pub use uinput::UinputKeyboard;
pub use xtest::XTestSender;

/// Linux input event codes for the keys we press.
pub const KEY_LEFTCTRL: u16 = 29;
pub const KEY_LEFTSHIFT: u16 = 42;
pub const KEY_V: u16 = 47;

/// The paste shortcut to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Combo {
    CtrlV,
    CtrlShiftV,
}

/// Key events for a combo as `(linux keycode, pressed)`: modifiers go down in
/// order, then everything is released in reverse.
pub fn key_events(c: Combo) -> Vec<(u16, bool)> {
    let keys: &[u16] = match c {
        Combo::CtrlV => &[KEY_LEFTCTRL, KEY_V],
        Combo::CtrlShiftV => &[KEY_LEFTCTRL, KEY_LEFTSHIFT, KEY_V],
    };
    keys.iter()
        .map(|&k| (k, true))
        .chain(keys.iter().rev().map(|&k| (k, false)))
        .collect()
}

/// Something that can type the paste shortcut.
pub trait KeySender: Send {
    fn send(&mut self, c: Combo) -> Result<(), String>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_shift_v_events() {
        assert_eq!(
            key_events(Combo::CtrlShiftV),
            vec![
                (29, true),
                (42, true),
                (47, true),
                (47, false),
                (42, false),
                (29, false)
            ]
        );
    }

    #[test]
    fn ctrl_v_events() {
        assert_eq!(
            key_events(Combo::CtrlV),
            vec![(29, true), (47, true), (47, false), (29, false)]
        );
    }
}
