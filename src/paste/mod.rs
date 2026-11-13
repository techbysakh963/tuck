// SPDX-License-Identifier: GPL-3.0-or-later
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

/// Offset between X11/xkb hardware keycodes and Linux input codes.
const XKB_OFFSET: u32 = 8;

/// The Linux input code of the key that types "v" in the active layout, from
/// the `(hardware keycode, group, level)` entries the toolkit reports for it.
/// The unshifted entry of the first layout wins; without a usable entry the
/// QWERTY position is used.
pub fn v_key_from_entries(entries: &[(u32, i32, i32)]) -> u16 {
    let usable =
        |&&(code, _, _): &&(u32, i32, i32)| (XKB_OFFSET + 1..=XKB_OFFSET + 255).contains(&code);
    entries
        .iter()
        .filter(usable)
        .min_by_key(|&&(_, group, level)| (level != 0, group))
        .map_or(KEY_V, |&(code, _, _)| (code - XKB_OFFSET) as u16)
}

/// Key events for a combo as `(linux keycode, pressed)`: modifiers go down in
/// order, then everything is released in reverse. `v` is the paste key.
pub fn key_events(c: Combo, v: u16) -> Vec<(u16, bool)> {
    let keys: &[u16] = match c {
        Combo::CtrlV => &[KEY_LEFTCTRL, v],
        Combo::CtrlShiftV => &[KEY_LEFTCTRL, KEY_LEFTSHIFT, v],
    };
    keys.iter()
        .map(|&k| (k, true))
        .chain(keys.iter().rev().map(|&k| (k, false)))
        .collect()
}

/// Something that can type the paste shortcut.
pub trait KeySender: Send {
    fn send(&mut self, c: Combo) -> Result<(), String>;

    /// Tells the sender which Linux input code types "v" right now; layouts
    /// move it. Senders that do not type keys ignore this.
    fn set_paste_key(&mut self, _linux_code: u16) {}
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ctrl_shift_v_events() {
        assert_eq!(
            key_events(Combo::CtrlShiftV, KEY_V),
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
            key_events(Combo::CtrlV, KEY_V),
            vec![(29, true), (47, true), (47, false), (29, false)]
        );
    }

    #[test]
    fn paste_key_follows_the_layout() {
        // QWERTY: keycode 55 is evdev 47.
        assert_eq!(v_key_from_entries(&[(55, 0, 0), (55, 0, 1)]), 47);
        // Dvorak puts "v" on the physical period key (evdev 52).
        assert_eq!(v_key_from_entries(&[(60, 0, 0), (60, 0, 1)]), 52);
        // Unshifted entry of the first group beats the others.
        assert_eq!(
            v_key_from_entries(&[(60, 1, 0), (61, 0, 1), (62, 0, 0)]),
            54
        );
        // Nothing usable falls back to QWERTY.
        assert_eq!(v_key_from_entries(&[]), KEY_V);
        assert_eq!(v_key_from_entries(&[(0, 0, 0), (4, 0, 0)]), KEY_V);
    }

    #[test]
    fn remapped_key_is_used_in_the_events() {
        assert_eq!(
            key_events(Combo::CtrlV, 52),
            vec![(29, true), (52, true), (52, false), (29, false)]
        );
    }
}
