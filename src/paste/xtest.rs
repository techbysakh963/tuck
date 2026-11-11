//! Paste keystrokes through the XTEST extension on X11 sessions.

use std::thread::sleep;
use std::time::Duration;

use x11rb::connection::Connection;
use x11rb::protocol::xproto::{KEY_PRESS_EVENT, KEY_RELEASE_EVENT};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

use super::{key_events, Combo, KeySender};

/// Offset between Linux input codes and X11 keycodes on evdev keymaps.
const EVDEV_OFFSET: u16 = 8;

const KEY_GAP: Duration = Duration::from_millis(8);

pub struct XTestSender {
    conn: RustConnection,
    root: u32,
}

impl XTestSender {
    pub fn connect() -> Result<Self, String> {
        let (conn, screen) =
            RustConnection::connect(None).map_err(|e| format!("X11 connect: {e}"))?;
        let root = conn.setup().roots[screen].root;
        conn.xtest_get_version(2, 2)
            .map_err(|e| e.to_string())?
            .reply()
            .map_err(|e| format!("XTEST unavailable: {e}"))?;
        Ok(Self { conn, root })
    }
}

impl KeySender for XTestSender {
    fn send(&mut self, c: Combo) -> Result<(), String> {
        let events = key_events(c);
        let last = events.len() - 1;
        for (i, (key, down)) in events.into_iter().enumerate() {
            let kind = if down {
                KEY_PRESS_EVENT
            } else {
                KEY_RELEASE_EVENT
            };
            let keycode = (key + EVDEV_OFFSET) as u8;
            self.conn
                .xtest_fake_input(kind, keycode, 0, self.root, 0, 0, 0)
                .map_err(|e| e.to_string())?;
            self.conn.flush().map_err(|e| e.to_string())?;
            if i != last {
                sleep(KEY_GAP);
            }
        }
        Ok(())
    }
}
