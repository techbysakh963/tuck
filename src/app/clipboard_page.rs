//! The Clipboard tab.

use std::rc::Rc;

use gtk4 as gtk;
use gtk4::prelude::*;
use libadwaita as adw;

use super::paste_flow::PasteMode;
use super::AppCtx;

pub struct ClipboardPage {
    pub root: gtk::Box,
}

impl ClipboardPage {
    pub fn new() -> Rc<Self> {
        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(
            &adw::StatusPage::builder()
                .icon_name("edit-paste-symbolic")
                .title("Nothing copied yet")
                .vexpand(true)
                .build(),
        );
        Rc::new(Self { root })
    }

    pub fn attach(self: &Rc<Self>, _ctx: &Rc<AppCtx>) {}
    pub fn set_query(&self, _query: &str) {}
    pub fn refresh(&self, _force: bool) {}
    pub fn move_selection(&self, _delta: i32) {}
    pub fn activate_selected(&self, _mode: PasteMode) {}
    pub fn toggle_pin_selected(&self) {}
    pub fn delete_selected(&self) {}
}
