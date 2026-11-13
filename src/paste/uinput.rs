// SPDX-License-Identifier: GPL-3.0-or-later
//! A persistent virtual keyboard on `/dev/uinput`, used on Wayland where no
//! other general way to type into another window exists.

use std::fs::{File, OpenOptions};
use std::io::Write;
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::thread::sleep;
use std::time::Duration;

use super::{key_events, Combo, KeySender, KEY_V};

const UI_SET_EVBIT: libc::c_ulong = 0x4004_5564;
const UI_SET_KEYBIT: libc::c_ulong = 0x4004_5565;
const UI_DEV_SETUP: libc::c_ulong = 0x405c_5503;
const UI_DEV_CREATE: libc::c_ulong = 0x5501;
const UI_DEV_DESTROY: libc::c_ulong = 0x5502;

/// Highest input code enabled on the device.
const MAX_KEY: u16 = 255;

const EV_SYN: u16 = 0;
const EV_KEY: u16 = 1;
const SYN_REPORT: u16 = 0;
const BUS_USB: u16 = 0x03;

/// Time between a key going down and the next event.
const KEY_GAP: Duration = Duration::from_millis(8);

/// Time the desktop needs to notice a freshly created device.
const SETTLE: Duration = Duration::from_millis(100);

#[repr(C)]
struct UinputSetup {
    bustype: u16,
    vendor: u16,
    product: u16,
    version: u16,
    name: [u8; 80],
    ff_effects_max: u32,
}

/// Virtual keyboard created once and kept for the life of the process.
pub struct UinputKeyboard {
    file: File,
    v_key: u16,
}

impl UinputKeyboard {
    pub fn create() -> Result<Self, String> {
        let file = OpenOptions::new()
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open("/dev/uinput")
            .map_err(|e| format!("cannot open /dev/uinput: {e}"))?;
        let fd = file.as_raw_fd();

        let ioctl = |request: libc::c_ulong, arg: libc::c_int, what: &str| {
            // SAFETY: plain integer argument ioctls on a descriptor we own.
            if unsafe { libc::ioctl(fd, request, arg) } < 0 {
                Err(format!(
                    "uinput {what}: {}",
                    std::io::Error::last_os_error()
                ))
            } else {
                Ok(())
            }
        };
        ioctl(UI_SET_EVBIT, EV_KEY as libc::c_int, "event bit")?;
        // Every regular key, because the one that types "v" depends on the layout.
        for key in 1..=MAX_KEY {
            ioctl(UI_SET_KEYBIT, key as libc::c_int, "key bit")?;
        }

        let mut name = [0u8; 80];
        let label = b"Tuck virtual keyboard";
        name[..label.len()].copy_from_slice(label);
        let setup = UinputSetup {
            bustype: BUS_USB,
            vendor: 0x1209,
            product: 0x0001,
            version: 1,
            name,
            ff_effects_max: 0,
        };
        // SAFETY: `setup` matches the kernel's `struct uinput_setup` layout
        // and outlives the call.
        if unsafe { libc::ioctl(fd, UI_DEV_SETUP, &setup) } < 0 {
            return Err(format!("uinput setup: {}", std::io::Error::last_os_error()));
        }
        // SAFETY: argument-less ioctl.
        if unsafe { libc::ioctl(fd, UI_DEV_CREATE) } < 0 {
            return Err(format!(
                "uinput create: {}",
                std::io::Error::last_os_error()
            ));
        }
        sleep(SETTLE);
        Ok(Self { file, v_key: KEY_V })
    }

    fn emit(&mut self, type_: u16, code: u16, value: i32) -> Result<(), String> {
        let event = libc::input_event {
            time: libc::timeval {
                tv_sec: 0,
                tv_usec: 0,
            },
            type_,
            code,
            value,
        };
        // SAFETY: `input_event` is plain old data; viewing it as bytes is
        // exactly how the kernel expects it.
        let bytes = unsafe {
            std::slice::from_raw_parts(
                (&event as *const libc::input_event).cast::<u8>(),
                std::mem::size_of::<libc::input_event>(),
            )
        };
        self.file
            .write_all(bytes)
            .map_err(|e| format!("uinput write: {e}"))
    }
}

impl KeySender for UinputKeyboard {
    fn set_paste_key(&mut self, linux_code: u16) {
        self.v_key = linux_code;
    }

    fn send(&mut self, c: Combo) -> Result<(), String> {
        let events = key_events(c, self.v_key);
        let last = events.len() - 1;
        for (i, (key, down)) in events.into_iter().enumerate() {
            self.emit(EV_KEY, key, i32::from(down))?;
            self.emit(EV_SYN, SYN_REPORT, 0)?;
            if i != last {
                sleep(KEY_GAP);
            }
        }
        Ok(())
    }
}

impl Drop for UinputKeyboard {
    fn drop(&mut self) {
        // SAFETY: argument-less ioctl on our descriptor.
        unsafe {
            libc::ioctl(self.file.as_raw_fd(), UI_DEV_DESTROY);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    #[ignore = "needs write access to /dev/uinput"]
    fn live_create_keyboard() {
        // Creates and destroys the device without typing anything.
        UinputKeyboard::create().expect("create virtual keyboard");
    }
}
