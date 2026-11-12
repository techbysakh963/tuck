//! The one-time offer to bind Super+V, and the shared "apply a shortcut" step.

use std::rc::Rc;

use libadwaita as adw;
use libadwaita::prelude::*;

use crate::settings::ShortcutChoice;
use crate::shortcut;

use super::AppCtx;

const MANUAL_HINT: &str = "Bind \"tuck --toggle\" to a key in your desktop's keyboard settings";
pub const TOGGLE_COMMAND: &str = "tuck --toggle";

const LOGIN_NOTE: &str = "Tuck starts when you log in; you can turn that off in Preferences.";

/// Heading and body of the first-run dialog for this desktop.
fn first_run_text(gnome_shortcuts: bool) -> (&'static str, String) {
    if gnome_shortcuts {
        (
            "Open Tuck with Super+V?",
            format!(
                "Super+V is GNOME's shortcut for the notification list. Using it for \
                 Tuck replaces that; Super+M still opens the notification list. You \
                 can change this later in Preferences.\n\n{LOGIN_NOTE}"
            ),
        )
    } else {
        (
            "Open Tuck with a shortcut",
            format!("Tuck cannot set a shortcut on this desktop. {MANUAL_HINT}.\n\n{LOGIN_NOTE}"),
        )
    }
}

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
/// nothing away from GNOME. Where Tuck cannot set a shortcut, it shows how to
/// bind one by hand instead.
pub fn present(ctx: &Rc<AppCtx>) {
    let gnome = shortcut::gnome_supported();
    let (heading, body) = first_run_text(gnome);
    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .body(body)
        .build();
    if gnome {
        dialog.add_responses(&[("ctrl", "Use Ctrl+Alt+V"), ("super", "Use Super+V")]);
        dialog.set_response_appearance("super", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("super"));
        dialog.set_close_response("ctrl");
    } else {
        dialog.add_responses(&[("close", "Close"), ("copy", "Copy command")]);
        dialog.set_response_appearance("copy", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("copy"));
        dialog.set_close_response("close");
    }

    let ctx_for_response = ctx.clone();
    dialog.connect_response(None, move |_, response| {
        let ctx = &ctx_for_response;
        if gnome {
            let choice = if response == "super" {
                ShortcutChoice::SuperV
            } else {
                ShortcutChoice::CtrlAltV
            };
            apply_choice(ctx, choice);
        } else if response == "copy" {
            ctx.window.window.clipboard().set_text(TOGGLE_COMMAND);
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_run_text_matches_the_desktop() {
        let (_, gnome) = first_run_text(true);
        assert!(gnome.contains("starts when you log in"));
        assert!(!gnome.contains(TOGGLE_COMMAND));
        let (_, other) = first_run_text(false);
        assert!(other.contains(TOGGLE_COMMAND));
        assert!(other.contains("starts when you log in"));
    }
}
