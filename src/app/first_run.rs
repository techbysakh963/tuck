//! The one-time offer to bind Super+V, and the shared "apply a shortcut" step.

use std::rc::Rc;

use libadwaita as adw;
use libadwaita::prelude::*;

use crate::settings::ShortcutChoice;
use crate::shortcut;

use super::AppCtx;

const MANUAL_HINT: &str = "Bind \"tuck --toggle\" in your desktop's keyboard settings";

fn save_settings(ctx: &AppCtx) {
    if let Err(e) = ctx.settings.borrow().save(&ctx.config_path) {
        log::error!("cannot save settings: {e}");
    }
}

/// Registers `choice` with the desktop and reports the outcome to the user.
/// Returns true when the shortcut is in place.
pub fn apply_choice(ctx: &AppCtx, choice: ShortcutChoice) -> bool {
    let result = shortcut::install_gnome(choice, &mut ctx.settings.borrow_mut());
    save_settings(ctx);
    match result {
        Ok(()) => true,
        Err(e) if e == "manual" => {
            ctx.window.add_toast(MANUAL_HINT);
            false
        }
        Err(e) => {
            log::warn!("cannot set the shortcut: {e}");
            ctx.window.add_toast("Could not set the shortcut");
            false
        }
    }
}

pub const MANUAL_SHORTCUT_TEXT: &str = MANUAL_HINT;

/// Asks which shortcut to use. Declining (Esc) selects Ctrl+Alt+V, which takes
/// nothing away from GNOME.
pub fn present(ctx: &Rc<AppCtx>) {
    let dialog = adw::AlertDialog::builder()
        .heading("Open Tuck with Super+V?")
        .body(
            "Super+V is GNOME's shortcut for the notification list. Using it for \
             Tuck replaces that; Super+M still opens the notification list. You \
             can change this later in Preferences.",
        )
        .build();
    dialog.add_responses(&[("ctrl", "Use Ctrl+Alt+V"), ("super", "Use Super+V")]);
    dialog.set_response_appearance("super", adw::ResponseAppearance::Suggested);
    dialog.set_default_response(Some("super"));
    dialog.set_close_response("ctrl");

    let ctx_for_response = ctx.clone();
    dialog.connect_response(None, move |_, response| {
        let choice = if response == "super" {
            ShortcutChoice::SuperV
        } else {
            ShortcutChoice::CtrlAltV
        };
        let ctx = &ctx_for_response;
        apply_choice(ctx, choice);
        {
            let mut settings = ctx.settings.borrow_mut();
            settings.first_run_done = true;
            if let Err(e) = crate::autostart::set_enabled(settings.start_at_login) {
                log::warn!("cannot set up start at login: {e}");
            }
        }
        save_settings(ctx);
    });
    dialog.present(Some(&ctx.window.window));
}
