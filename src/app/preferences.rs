// SPDX-License-Identifier: GPL-3.0-or-later
//! The Preferences dialog.

use std::rc::Rc;

use gtk4 as gtk;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::privacy::{compile, PrivacyFilter};
use crate::settings::ShortcutChoice;
use crate::shortcut;

use super::first_run::{apply_choice, MANUAL_SHORTCUT_TEXT, TOGGLE_COMMAND};
use super::{prune_by_age, schedule_save, AppCtx};

const AUTO_CLEAR_CHOICES: [(&str, Option<u32>); 4] = [
    ("Never", None),
    ("After 1 day", Some(1)),
    ("After 7 days", Some(7)),
    ("After 30 days", Some(30)),
];

fn save(ctx: &AppCtx) {
    if let Err(e) = ctx.settings.borrow().save(&ctx.config_path) {
        log::error!("cannot save settings: {e}");
    }
}

fn shortcut_subtitle(ctx: &AppCtx) -> String {
    if shortcut::gnome_supported() {
        shortcut::describe(ctx.settings.borrow().shortcut).to_string()
    } else {
        MANUAL_SHORTCUT_TEXT.to_string()
    }
}

fn history_group(ctx: &Rc<AppCtx>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("History").build();

    let max_items = adw::SpinRow::with_range(10.0, 1000.0, 10.0);
    max_items.set_title("Items to keep");
    max_items.set_subtitle("Pinned items count too");
    max_items.set_value(ctx.settings.borrow().max_items as f64);
    let c = ctx.clone();
    max_items.connect_value_notify(move |row| {
        let n = row.value() as usize;
        c.settings.borrow_mut().max_items = n;
        c.store.borrow_mut().set_max_items(n);
        save(&c);
        schedule_save(&c);
        c.window.clipboard.refresh(true);
    });
    group.add(&max_items);

    let names: Vec<&str> = AUTO_CLEAR_CHOICES.iter().map(|(n, _)| *n).collect();
    let auto_clear = adw::ComboRow::builder()
        .title("Clear history automatically")
        .subtitle("Pinned items are kept")
        .model(&gtk::StringList::new(&names))
        .build();
    let current = ctx.settings.borrow().auto_clear_days;
    if let Some(i) = AUTO_CLEAR_CHOICES.iter().position(|(_, d)| *d == current) {
        auto_clear.set_selected(i as u32);
    }
    let c = ctx.clone();
    auto_clear.connect_selected_notify(move |row| {
        if let Some((_, days)) = AUTO_CLEAR_CHOICES.get(row.selected() as usize) {
            c.settings.borrow_mut().auto_clear_days = *days;
            save(&c);
            prune_by_age(&c);
            schedule_save(&c);
            c.window.clipboard.refresh(true);
        }
    });
    group.add(&auto_clear);
    group
}

fn apply_exclusions(ctx: &AppCtx) {
    let patterns = ctx.settings.borrow().exclusions.clone();
    *ctx.filter.borrow_mut() = PrivacyFilter::new(&patterns);
    save(ctx);
}

fn privacy_group(ctx: &Rc<AppCtx>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder()
        .title("Privacy")
        .description(
            "Copies from password managers are never saved. Text matching a \
             pattern below is skipped too.",
        )
        .build();

    let pause = adw::SwitchRow::builder()
        .title("Pause recording")
        .active(ctx.settings.borrow().paused)
        .build();
    let c = ctx.clone();
    pause.connect_active_notify(move |row| {
        // The app action owns the state and the saving.
        if row.is_active() != c.settings.borrow().paused {
            c.app.activate_action("pause", None);
        }
    });
    group.add(&pause);

    let add = adw::EntryRow::builder()
        .title("Add exclusion pattern (regular expression)")
        .show_apply_button(true)
        .build();
    group.add(&add);

    let rows: Rc<std::cell::RefCell<Vec<adw::ActionRow>>> = Rc::default();
    fill_patterns(ctx, &group, &rows);

    let (c, g, r) = (ctx.clone(), group.clone(), rows.clone());
    add.connect_apply(move |entry| {
        let text = entry.text().to_string();
        if text.is_empty() {
            return;
        }
        if compile(&text).is_err() {
            c.window
                .add_toast("That is not a valid regular expression, or it is too large");
            entry.add_css_class("error");
            return;
        }
        entry.remove_css_class("error");
        {
            let mut settings = c.settings.borrow_mut();
            if !settings.exclusions.contains(&text) {
                settings.exclusions.push(text);
            }
        }
        entry.set_text("");
        apply_exclusions(&c);
        fill_patterns(&c, &g, &r);
    });
    group
}

/// Rebuilds the pattern rows under the entry; removal rebuilds again.
fn fill_patterns(
    ctx: &Rc<AppCtx>,
    group: &adw::PreferencesGroup,
    rows: &Rc<std::cell::RefCell<Vec<adw::ActionRow>>>,
) {
    for row in rows.borrow_mut().drain(..) {
        group.remove(&row);
    }
    let patterns = ctx.settings.borrow().exclusions.clone();
    for pattern in patterns {
        let row = adw::ActionRow::builder().title(&pattern).build();
        row.add_css_class("monospace");
        let remove = gtk::Button::from_icon_name("user-trash-symbolic");
        remove.set_valign(gtk::Align::Center);
        remove.add_css_class("flat");
        remove.set_tooltip_text(Some("Remove"));
        row.add_suffix(&remove);
        let (c, g, r, p) = (ctx.clone(), group.clone(), rows.clone(), pattern);
        remove.connect_clicked(move |_| {
            c.settings.borrow_mut().exclusions.retain(|x| x != &p);
            apply_exclusions(&c);
            fill_patterns(&c, &g, &r);
        });
        group.add(&row);
        rows.borrow_mut().push(row);
    }
}

fn choose_shortcut(ctx: &Rc<AppCtx>, row: &adw::ActionRow) {
    let dialog = adw::AlertDialog::builder()
        .heading("Choose a shortcut")
        .body("Super+V replaces GNOME's notification-list shortcut.")
        .build();
    dialog.add_responses(&[
        ("cancel", "Cancel"),
        ("ctrl", "Ctrl+Alt+V"),
        ("super", "Super+V"),
    ]);
    dialog.set_close_response("cancel");
    let (c, row) = (ctx.clone(), row.clone());
    dialog.connect_response(None, move |_, response| {
        let choice = match response {
            "super" => ShortcutChoice::SuperV,
            "ctrl" => ShortcutChoice::CtrlAltV,
            _ => return,
        };
        apply_choice(&c, choice);
        row.set_subtitle(&shortcut_subtitle(&c));
    });
    dialog.present(Some(&ctx.window.window));
}

fn keyboard_group(ctx: &Rc<AppCtx>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("Keyboard").build();
    let row = adw::ActionRow::builder()
        .title("Shortcut to open Tuck")
        .subtitle(shortcut_subtitle(ctx))
        .build();

    if !shortcut::gnome_supported() {
        let copy = gtk::Button::with_label("Copy command");
        copy.set_valign(gtk::Align::Center);
        let c = ctx.clone();
        copy.connect_clicked(move |_| {
            c.window.window.clipboard().set_text(TOGGLE_COMMAND);
            c.window.add_toast("Command copied");
        });
        row.add_suffix(&copy);
        group.add(&row);
        return group;
    }

    let change = gtk::Button::with_label("Change");
    change.set_valign(gtk::Align::Center);
    let reset = gtk::Button::with_label("Reset shortcut");
    reset.set_valign(gtk::Align::Center);
    row.add_suffix(&change);
    row.add_suffix(&reset);

    let (c, r) = (ctx.clone(), row.clone());
    change.connect_clicked(move |_| choose_shortcut(&c, &r));
    let (c, r) = (ctx.clone(), row.clone());
    reset.connect_clicked(move |_| {
        shortcut::reset_gnome(&mut c.settings.borrow_mut());
        save(&c);
        r.set_subtitle(&shortcut_subtitle(&c));
        c.window
            .add_toast("Shortcut removed, GNOME's original restored");
    });
    group.add(&row);
    group
}

fn general_group(ctx: &Rc<AppCtx>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::builder().title("General").build();
    let login = adw::SwitchRow::builder()
        .title("Start at login")
        // The autostart file is the truth; the user may have removed it by hand.
        .active(crate::autostart::is_enabled())
        .build();
    let c = ctx.clone();
    login.connect_active_notify(move |row| {
        let on = row.is_active();
        if let Err(e) = crate::autostart::set_enabled(on) {
            log::warn!("cannot change start at login: {e}");
            c.window.add_toast("Could not change start at login");
            return;
        }
        c.settings.borrow_mut().start_at_login = on;
        save(&c);
    });
    group.add(&login);

    let tray = adw::SwitchRow::builder()
        .title("Show icon in top bar")
        .subtitle("Needs the AppIndicator extension, which Ubuntu enables by default")
        .active(ctx.settings.borrow().show_tray_icon)
        .build();
    let c = ctx.clone();
    tray.connect_active_notify(move |row| {
        c.settings.borrow_mut().show_tray_icon = row.is_active();
        save(&c);
        c.sync_tray();
    });
    group.add(&tray);
    group
}

fn about_group(ctx: &Rc<AppCtx>) -> adw::PreferencesGroup {
    let group = adw::PreferencesGroup::new();
    let row = adw::ActionRow::builder()
        .title("About Tuck")
        .activatable(true)
        .build();
    row.add_suffix(&gtk::Image::from_icon_name("go-next-symbolic"));
    let c = ctx.clone();
    row.connect_activated(move |_| c.app.activate_action("about", None));
    group.add(&row);
    group
}

pub fn present(ctx: &Rc<AppCtx>) {
    let page = adw::PreferencesPage::new();
    page.add(&history_group(ctx));
    page.add(&privacy_group(ctx));
    page.add(&keyboard_group(ctx));
    page.add(&general_group(ctx));
    page.add(&about_group(ctx));

    let dialog = adw::PreferencesDialog::new();
    dialog.set_title("Preferences");
    dialog.add(&page);
    dialog.present(Some(&ctx.window.window));
}
