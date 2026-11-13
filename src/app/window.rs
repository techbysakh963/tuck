// SPDX-License-Identifier: GPL-3.0-or-later
//! The popup window: tabs, search, menu and the page stack.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use gtk4 as gtk;
use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use super::clipboard_page::ClipboardPage;
use super::grid_page::GridPage;
use super::paste_flow::PasteMode;
use super::symbols::{emoji_entries, symbol_entries};
use super::AppCtx;

const WIDTH: i32 = 380;
const HEIGHT: i32 = 600;

const PAGE_CLIPBOARD: &str = "clipboard";
const PAGE_EMOJI: &str = "emoji";
const PAGE_SYMBOLS: &str = "symbols";

pub struct PopupWindow {
    pub window: adw::ApplicationWindow,
    pub clipboard: Rc<ClipboardPage>,
    pub emoji: Rc<GridPage>,
    pub symbols: Rc<GridPage>,
    pub search: gtk::SearchEntry,
    stack: gtk::Stack,
    menu_button: gtk::MenuButton,
    tab_buttons: Vec<gtk::ToggleButton>,
    overlay: adw::ToastOverlay,
}

fn tab(icon: &str, label: &str) -> gtk::ToggleButton {
    let content = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    content.set_halign(gtk::Align::Center);
    content.append(&gtk::Image::from_icon_name(icon));
    content.append(&gtk::Label::new(Some(label)));
    let button = gtk::ToggleButton::new();
    button.set_child(Some(&content));
    button.set_hexpand(true);
    button
}

/// Sends the search text to the page that is showing.
fn route_query(
    page: &str,
    query: &str,
    clipboard: &ClipboardPage,
    emoji: &GridPage,
    symbols: &GridPage,
) {
    match page {
        PAGE_EMOJI => emoji.set_query(query),
        PAGE_SYMBOLS => symbols.set_query(query),
        _ => clipboard.set_query(query),
    }
}

/// The popup's own shortcuts apply only while nothing else (a dialog or the
/// menu) is in front; otherwise Enter, Delete and Esc belong to that.
fn popup_keys_apply(dialog_open: bool, menu_open: bool) -> bool {
    !dialog_open && !menu_open
}

/// The compositor's focus state can flicker while a menu or dialog opens or
/// closes. Only a loss that lasts this long (the user went to another
/// application) closes the popup.
const FOCUS_LOSS_GRACE: std::time::Duration = std::time::Duration::from_millis(250);

/// Every way of hiding the popup goes through here so that the menu is never
/// left open behind a hidden window.
fn hide_popup(window: &adw::ApplicationWindow, menu_button: &gtk::MenuButton) {
    menu_button.popdown();
    window.set_visible(false);
}

fn tab_title(page: &str) -> &'static str {
    match page {
        PAGE_EMOJI => "Emoji",
        PAGE_SYMBOLS => "Symbols",
        _ => "Clipboard",
    }
}

/// The tab a shortcut switches to: Ctrl+Tab and Ctrl+Shift+Tab cycle,
/// Alt+1..3 pick one. `None` when the key is not a tab shortcut.
fn tab_for_key(
    key: gtk::gdk::Key,
    ctrl: bool,
    shift: bool,
    alt: bool,
    current: usize,
    count: usize,
) -> Option<usize> {
    use gtk::gdk::Key;
    if count == 0 {
        return None;
    }
    if ctrl && !alt && key == Key::Tab && !shift {
        return Some((current + 1) % count);
    }
    if ctrl && !alt && (key == Key::ISO_Left_Tab || (key == Key::Tab && shift)) {
        return Some((current + count - 1) % count);
    }
    if alt && !ctrl && !shift {
        let n = match key {
            Key::_1 | Key::KP_1 => 0,
            Key::_2 | Key::KP_2 => 1,
            Key::_3 | Key::KP_3 => 2,
            _ => return None,
        };
        return (n < count).then_some(n);
    }
    None
}

fn menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let main = gio::Menu::new();
    main.append(Some("Preferences"), Some("app.preferences"));
    main.append(Some("Pause recording"), Some("app.pause"));
    main.append(Some("Clear history"), Some("app.clear"));
    main.append(Some("About Tuck"), Some("app.about"));
    menu.append_section(None, &main);
    let quit = gio::Menu::new();
    quit.append(Some("Quit Tuck"), Some("app.quit"));
    menu.append_section(None, &quit);
    menu
}

impl PopupWindow {
    pub fn new(app: &adw::Application) -> Self {
        let clipboard = ClipboardPage::new();
        let emoji = GridPage::new(emoji_entries, true);
        let symbols = GridPage::new(|| symbol_entries().to_vec(), false);
        let search = gtk::SearchEntry::new();

        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        // Sized by the page showing, so the unused grids are never measured.
        stack.set_hhomogeneous(false);
        stack.set_vhomogeneous(false);
        stack.add_named(&clipboard.root, Some(PAGE_CLIPBOARD));
        stack.add_named(&emoji.root, Some(PAGE_EMOJI));
        stack.add_named(&symbols.root, Some(PAGE_SYMBOLS));

        let tabs = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        tabs.add_css_class("linked");
        tabs.set_hexpand(true);
        tabs.set_homogeneous(true);
        let pages = [
            (tab("edit-paste-symbolic", "Clipboard"), PAGE_CLIPBOARD),
            (tab("face-smile-symbolic", "Emoji"), PAGE_EMOJI),
            (tab("insert-text-symbolic", "Symbols"), PAGE_SYMBOLS),
        ];
        for (i, (button, name)) in pages.iter().enumerate() {
            button.set_tooltip_text(Some(&format!(
                "{} (Alt+{}, Ctrl+Tab for the next)",
                tab_title(name),
                i + 1
            )));
            if i > 0 {
                button.set_group(Some(&pages[0].0));
            }
            let stack = stack.clone();
            let name = *name;
            let (search, clipboard, emoji, symbols) = (
                search.clone(),
                clipboard.clone(),
                emoji.clone(),
                symbols.clone(),
            );
            button.connect_toggled(move |b| {
                if b.is_active() {
                    stack.set_visible_child_name(name);
                    // The search text carries over to the page that opens.
                    route_query(name, search.text().as_str(), &clipboard, &emoji, &symbols);
                    match name {
                        PAGE_EMOJI => emoji.ensure_built(),
                        PAGE_SYMBOLS => symbols.ensure_built(),
                        _ => {}
                    }
                    search.grab_focus();
                }
            });
            tabs.append(button);
        }
        pages[0].0.set_active(true);

        let menu_button = gtk::MenuButton::builder()
            .icon_name("open-menu-symbolic")
            .menu_model(&menu())
            .tooltip_text("Main menu")
            .build();
        menu_button.add_css_class("flat");

        let header = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        header.set_margin_start(12);
        header.set_margin_end(12);
        header.set_margin_top(12);
        header.append(&tabs);
        header.append(&menu_button);
        let handle = gtk::WindowHandle::new();
        handle.set_child(Some(&header));

        search.set_placeholder_text(Some("Search"));
        search.set_margin_start(12);
        search.set_margin_end(12);
        search.set_margin_top(12);
        search.set_margin_bottom(6);

        let body = gtk::Box::new(gtk::Orientation::Vertical, 0);
        body.append(&handle);
        body.append(&search);
        body.append(&stack);
        search.set_key_capture_widget(Some(&body));

        let overlay = adw::ToastOverlay::new();
        overlay.set_child(Some(&body));

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("Tuck")
            .default_width(WIDTH)
            .default_height(HEIGHT)
            .resizable(false)
            .decorated(false)
            .content(&overlay)
            .build();

        // The window lives for the whole session: closing only hides it.
        let close_menu = menu_button.clone();
        window.connect_close_request(move |w| {
            hide_popup(w, &close_menu);
            glib::Propagation::Stop
        });

        let (stack_c, clipboard_c, emoji_c, symbols_c) = (
            stack.clone(),
            Rc::downgrade(&clipboard),
            Rc::downgrade(&emoji),
            Rc::downgrade(&symbols),
        );
        search.connect_search_changed(move |e| {
            if let (Some(c), Some(em), Some(sy), Some(name)) = (
                clipboard_c.upgrade(),
                emoji_c.upgrade(),
                symbols_c.upgrade(),
                stack_c.visible_child_name(),
            ) {
                route_query(&name, e.text().as_str(), &c, &em, &sy);
            }
        });

        Self {
            window,
            clipboard,
            emoji,
            symbols,
            search,
            stack,
            menu_button,
            tab_buttons: pages.iter().map(|(b, _)| b.clone()).collect(),
            overlay,
        }
    }

    /// Wires focus-loss hiding and the keyboard shortcuts; call once.
    pub fn attach(&self, ctx: &Rc<AppCtx>) {
        self.clipboard.attach(ctx);
        self.emoji.attach(ctx);
        self.symbols.attach(ctx);

        // Dismissal follows the compositor's focus state of the toplevel, not
        // the keyboard focus: on Wayland a menu of ours is its own surface and
        // takes the keyboard, while the toplevel stays focused as far as the
        // compositor is concerned.
        let generation = Rc::new(Cell::new(0u64));
        {
            let generation = generation.clone();
            // A pending check must never survive a hide and a later show.
            self.window
                .connect_unmap(move |_| generation.set(generation.get().wrapping_add(1)));
        }
        let tracked: Rc<RefCell<Option<(gtk::gdk::Toplevel, glib::SignalHandlerId)>>> =
            Rc::default();
        let (window, menu_button) = (self.window.clone(), self.menu_button.clone());
        self.window.connect_map(move |w| {
            if let Some((old, id)) = tracked.borrow_mut().take() {
                old.disconnect(id);
            }
            let Some(top) = w
                .surface()
                .and_then(|s| s.dynamic_cast::<gtk::gdk::Toplevel>().ok())
            else {
                return;
            };
            let focused = Cell::new(top.state().contains(gtk::gdk::ToplevelState::FOCUSED));
            let (generation, window, menu_button) =
                (generation.clone(), window.clone(), menu_button.clone());
            let id = top.connect_state_notify(move |top| {
                let now = top.state().contains(gtk::gdk::ToplevelState::FOCUSED);
                if focused.replace(now) == now {
                    return;
                }
                log::info!("popup focus: {}", if now { "gained" } else { "lost" });
                let ticket = generation.get().wrapping_add(1);
                generation.set(ticket);
                if now {
                    return;
                }
                let (generation, window, menu_button, top) = (
                    generation.clone(),
                    window.clone(),
                    menu_button.clone(),
                    top.downgrade(),
                );
                glib::timeout_add_local_once(FOCUS_LOSS_GRACE, move || {
                    let Some(top) = top.upgrade() else { return };
                    if generation.get() == ticket
                        && window.is_mapped()
                        && !top.state().contains(gtk::gdk::ToplevelState::FOCUSED)
                    {
                        log::info!("popup hidden: focus left the window");
                        hide_popup(&window, &menu_button);
                    }
                });
            });
            *tracked.borrow_mut() = Some((top, id));
        });

        let keys = gtk::EventControllerKey::new();
        keys.set_propagation_phase(gtk::PropagationPhase::Capture);
        let ctx = Rc::downgrade(ctx);
        keys.connect_key_pressed(move |_, key, _, state| {
            let Some(ctx) = ctx.upgrade() else {
                return glib::Propagation::Proceed;
            };
            if ctx.window.handle_key(key, state) {
                glib::Propagation::Stop
            } else {
                glib::Propagation::Proceed
            }
        });
        self.window.add_controller(keys);
    }

    fn on_clipboard_page(&self) -> bool {
        self.stack.visible_child_name().as_deref() == Some(PAGE_CLIPBOARD)
    }

    fn handle_key(&self, key: gtk::gdk::Key, state: gtk::gdk::ModifierType) -> bool {
        use gtk::gdk::{Key, ModifierType};
        let menu_open = self.menu_button.popover().is_some_and(|p| p.is_visible());
        if !popup_keys_apply(self.window.visible_dialog().is_some(), menu_open) {
            return false;
        }
        let ctrl = state.contains(ModifierType::CONTROL_MASK);
        let shift = state.contains(ModifierType::SHIFT_MASK);
        let alt = state.contains(ModifierType::ALT_MASK);
        let current = self
            .tab_buttons
            .iter()
            .position(|b| b.is_active())
            .unwrap_or(0);
        if let Some(next) = tab_for_key(key, ctrl, shift, alt, current, self.tab_buttons.len()) {
            self.tab_buttons[next].set_active(true);
            return true;
        }
        if key == Key::Escape {
            self.hide();
            return true;
        }
        if !self.on_clipboard_page() {
            let grid = if self.stack.visible_child_name().as_deref() == Some(PAGE_EMOJI) {
                &self.emoji
            } else {
                &self.symbols
            };
            return match key {
                Key::Down if self.search.has_focus() || self.search.is_focus() => {
                    grid.focus_grid();
                    true
                }
                Key::Return | Key::KP_Enter if self.search.has_focus() => {
                    grid.activate_first();
                    true
                }
                _ => false,
            };
        }
        match key {
            Key::Down => self.clipboard.move_selection(1),
            Key::Up => self.clipboard.move_selection(-1),
            Key::Return | Key::KP_Enter => {
                let mode = if shift {
                    PasteMode::ForceTerminal
                } else {
                    PasteMode::Normal
                };
                self.clipboard.activate_selected(mode);
            }
            Key::p | Key::P if ctrl => self.clipboard.toggle_pin_selected(),
            Key::Delete | Key::KP_Delete => {
                // Inside the query, Delete edits text; only at its end does it
                // have nothing to delete there and removes the item instead.
                let at_end = self.search.position() == self.search.text().chars().count() as i32;
                if !at_end {
                    return false;
                }
                self.clipboard.delete_selected();
            }
            _ => return false,
        }
        true
    }

    /// Resets transient state and focuses the search entry before showing.
    pub fn prepare_to_show(&self) {
        self.search.set_text("");
        self.emoji.reset();
        self.symbols.reset();
        self.clipboard.refresh(true);
    }

    pub fn present(&self) {
        // A menu left open by an earlier hide would swallow the next click.
        self.menu_button.popdown();
        self.window.present();
        if self.window.visible_dialog().is_none() {
            self.search.grab_focus();
        }
    }

    pub fn hide(&self) {
        hide_popup(&self.window, &self.menu_button);
    }

    pub fn is_visible(&self) -> bool {
        self.window.is_visible()
    }

    pub fn add_toast(&self, message: &str) {
        self.overlay.add_toast(adw::Toast::new(message));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_shortcuts() {
        use gtk::gdk::Key;
        assert_eq!(tab_for_key(Key::Tab, true, false, false, 0, 3), Some(1));
        assert_eq!(tab_for_key(Key::Tab, true, false, false, 2, 3), Some(0));
        assert_eq!(
            tab_for_key(Key::ISO_Left_Tab, true, true, false, 0, 3),
            Some(2)
        );
        assert_eq!(tab_for_key(Key::_3, false, false, true, 0, 3), Some(2));
        assert_eq!(tab_for_key(Key::_4, false, false, true, 0, 3), None);
        assert_eq!(tab_for_key(Key::_1, false, false, false, 1, 3), None);
        assert_eq!(tab_for_key(Key::Tab, false, false, false, 1, 3), None);
    }

    #[test]
    fn popup_keys_stand_aside_for_dialogs_and_menu() {
        assert!(popup_keys_apply(false, false));
        assert!(!popup_keys_apply(true, false));
        assert!(!popup_keys_apply(false, true));
    }
}
