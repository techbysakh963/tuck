//! The optional top-bar icon (StatusNotifierItem). It runs on its own thread,
//! only reacts to D-Bus calls, and hands every click to the main loop as a
//! command, so nothing here touches the UI.

use ksni::blocking::{Handle, TrayMethods};
use ksni::menu::{CheckmarkItem, StandardItem};
use ksni::MenuItem;

const APP_ID: &str = "io.github.techbysakh963.Tuck";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCommand {
    Toggle,
    Preferences,
    TogglePause,
    Quit,
}

struct TuckTray {
    tx: async_channel::Sender<TrayCommand>,
    paused: bool,
}

impl TuckTray {
    fn send(&self, command: TrayCommand) {
        // Unbounded channel: this only fails once the app is shutting down.
        let _ = self.tx.try_send(command);
    }
}

impl ksni::Tray for TuckTray {
    fn id(&self) -> String {
        APP_ID.into()
    }

    fn title(&self) -> String {
        "Tuck".into()
    }

    fn icon_name(&self) -> String {
        format!("{APP_ID}-symbolic")
    }

    fn activate(&mut self, _x: i32, _y: i32) {
        self.send(TrayCommand::Toggle);
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        vec![
            StandardItem {
                label: "Open Tuck".into(),
                activate: Box::new(|t: &mut Self| t.send(TrayCommand::Toggle)),
                ..Default::default()
            }
            .into(),
            CheckmarkItem {
                label: "Pause recording".into(),
                checked: self.paused,
                activate: Box::new(|t: &mut Self| t.send(TrayCommand::TogglePause)),
                ..Default::default()
            }
            .into(),
            StandardItem {
                label: "Preferences".into(),
                activate: Box::new(|t: &mut Self| t.send(TrayCommand::Preferences)),
                ..Default::default()
            }
            .into(),
            MenuItem::Separator,
            StandardItem {
                label: "Quit".into(),
                activate: Box::new(|t: &mut Self| t.send(TrayCommand::Quit)),
                ..Default::default()
            }
            .into(),
        ]
    }
}

/// A running icon. Dropping it does not remove the icon; call `remove`.
pub struct Tray {
    handle: Handle<TuckTray>,
}

impl Tray {
    /// Shows the icon. `None` when no StatusNotifier host is running (a plain
    /// GNOME session without the AppIndicator extension), which is not an error.
    pub fn show(tx: async_channel::Sender<TrayCommand>, paused: bool) -> Option<Tray> {
        match (TuckTray { tx, paused }).spawn() {
            Ok(handle) => Some(Tray { handle }),
            Err(e) => {
                log::info!("no top-bar icon: {e}");
                None
            }
        }
    }

    pub fn set_paused(&self, paused: bool) {
        self.handle.update(|t: &mut TuckTray| t.paused = paused);
    }

    pub fn remove(self) {
        self.handle.shutdown().wait();
    }
}
