//! The popup window: tabs, search, menu and the page stack.

use std::rc::Rc;

use gtk4 as gtk;
use gtk4::gio;
use gtk4::glib;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use super::clipboard_page::ClipboardPage;
use super::paste_flow::PasteMode;
use super::AppCtx;

const WIDTH: i32 = 380;
const HEIGHT: i32 = 600;

const PAGE_CLIPBOARD: &str = "clipboard";
const PAGE_EMOJI: &str = "emoji";
const PAGE_SYMBOLS: &str = "symbols";

pub struct PopupWindow {
    pub window: adw::ApplicationWindow,
    pub clipboard: Rc<ClipboardPage>,
    pub search: gtk::SearchEntry,
    stack: gtk::Stack,
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

fn placeholder(icon: &str, title: &str) -> adw::StatusPage {
    adw::StatusPage::builder()
        .icon_name(icon)
        .title(title)
        .vexpand(true)
        .build()
}

fn menu() -> gio::Menu {
    let menu = gio::Menu::new();
    let main = gio::Menu::new();
    main.append(Some("Preferences"), Some("app.preferences"));
    main.append(Some("Pause recording"), Some("app.pause"));
    main.append(Some("Clear all"), Some("app.clear"));
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

        let stack = gtk::Stack::new();
        stack.set_vexpand(true);
        stack.add_named(&clipboard.root, Some(PAGE_CLIPBOARD));
        stack.add_named(
            &placeholder("face-smile-symbolic", "Emoji"),
            Some(PAGE_EMOJI),
        );
        stack.add_named(
            &placeholder("insert-text-symbolic", "Symbols"),
            Some(PAGE_SYMBOLS),
        );

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
            if i > 0 {
                button.set_group(Some(&pages[0].0));
            }
            let stack = stack.clone();
            let name = *name;
            button.connect_toggled(move |b| {
                if b.is_active() {
                    stack.set_visible_child_name(name);
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

        let search = gtk::SearchEntry::new();
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
        window.connect_close_request(|w| {
            w.set_visible(false);
            glib::Propagation::Stop
        });

        let page = Rc::downgrade(&clipboard);
        search.connect_search_changed(move |e| {
            if let Some(page) = page.upgrade() {
                page.set_query(e.text().as_str());
            }
        });

        Self {
            window,
            clipboard,
            search,
            stack,
            overlay,
        }
    }

    /// Wires focus-loss hiding and the keyboard shortcuts; call once.
    pub fn attach(&self, ctx: &Rc<AppCtx>) {
        self.clipboard.attach(ctx);

        // Losing focus closes the popup, except while one of our own dialogs
        // (About, Preferences) has it.
        self.window.connect_is_active_notify(|w| {
            if !w.is_active() && w.is_visible() && w.visible_dialog().is_none() {
                w.set_visible(false);
            }
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
        let ctrl = state.contains(ModifierType::CONTROL_MASK);
        let shift = state.contains(ModifierType::SHIFT_MASK);
        if key == Key::Escape {
            self.window.set_visible(false);
            return true;
        }
        if !self.on_clipboard_page() {
            return false;
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
        self.clipboard.refresh(true);
    }

    pub fn present(&self) {
        self.window.present();
        self.search.grab_focus();
    }

    pub fn hide(&self) {
        self.window.set_visible(false);
    }

    pub fn is_visible(&self) -> bool {
        self.window.is_visible()
    }

    pub fn add_toast(&self, message: &str) {
        self.overlay.add_toast(adw::Toast::new(message));
    }
}
