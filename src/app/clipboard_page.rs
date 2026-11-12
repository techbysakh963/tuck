//! The Clipboard tab: Pinned and Recent lists, search and keyboard handling.

use std::cell::RefCell;
use std::rc::{Rc, Weak};

use gtk4 as gtk;
use gtk4::prelude::*;
use libadwaita as adw;

use crate::model::{ClipContent, ClipItem, ItemId};

use super::paste_flow::{paste_item, PasteMode};
use super::{now_ms, schedule_save, AppCtx};

/// Upper bound on rendered Recent rows; search still covers all of history.
const MAX_RECENT_ROWS: usize = 200;

/// Items matching `query`: every whitespace-separated term must occur,
/// case-insensitively, in the start of the text (its first `PREVIEW_CHARS` characters). Images match the term "image".
pub fn filter_items<'a>(items: &'a [ClipItem], query: &str) -> Vec<&'a ClipItem> {
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    items
        .iter()
        .filter(|item| {
            let haystack = match &item.content {
                ClipContent::Text { text, .. } => text.head().to_lowercase(),
                ClipContent::Image { .. } => "image".to_string(),
            };
            terms.iter().all(|t| haystack.contains(t.as_str()))
        })
        .collect()
}

/// Short human wording for how long ago `then_ms` was.
pub fn relative_time(now_ms: i64, then_ms: i64) -> String {
    let secs = (now_ms - then_ms).max(0) / 1000;
    let mins = secs / 60;
    let hours = mins / 60;
    let days = hours / 24;
    if secs < 60 {
        "just now".into()
    } else if mins < 60 {
        format!("{mins} min ago")
    } else if hours < 24 {
        format!("{hours} h ago")
    } else if days < 2 {
        "yesterday".into()
    } else {
        format!("{days} days ago")
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Section {
    Pinned,
    Recent,
}

struct Entry {
    id: ItemId,
    section: Section,
    row: gtk::ListBoxRow,
}

#[derive(Default)]
struct State {
    query: String,
    entries: Vec<Entry>,
    selected: Option<ItemId>,
    signature: Option<Vec<(ItemId, bool)>>,
}

pub struct ClipboardPage {
    pub root: gtk::Box,
    scroller: gtk::ScrolledWindow,
    content: gtk::Box,
    pinned_heading: gtk::Label,
    pinned_list: gtk::ListBox,
    recent_heading: gtk::Label,
    recent_list: gtk::ListBox,
    empty: adw::StatusPage,
    state: RefCell<State>,
    ctx: RefCell<Weak<AppCtx>>,
}

fn heading(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.add_css_class("heading");
    label.set_xalign(0.0);
    label.set_margin_top(6);
    label
}

fn list() -> gtk::ListBox {
    let list = gtk::ListBox::new();
    list.add_css_class("boxed-list");
    list.set_selection_mode(gtk::SelectionMode::Single);
    list.set_activate_on_single_click(true);
    list
}

impl ClipboardPage {
    pub fn new() -> Rc<Self> {
        let pinned_heading = heading("Pinned");
        let recent_heading = heading("Recent");
        let pinned_list = list();
        let recent_list = list();
        let empty = adw::StatusPage::builder()
            .icon_name("edit-paste-symbolic")
            .title("Nothing here")
            .vexpand(true)
            .build();

        let content = gtk::Box::new(gtk::Orientation::Vertical, 6);
        content.set_margin_start(12);
        content.set_margin_end(12);
        content.set_margin_top(6);
        content.set_margin_bottom(12);
        content.append(&pinned_heading);
        content.append(&pinned_list);
        content.append(&recent_heading);
        content.append(&recent_list);
        content.append(&empty);

        let scroller = gtk::ScrolledWindow::builder()
            .child(&content)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();

        let clear = gtk::Button::with_label("Clear all");
        clear.add_css_class("flat");
        clear.set_action_name(Some("app.clear"));
        let hint = gtk::Label::new(Some("Enter to paste"));
        hint.add_css_class("dim-label");
        hint.add_css_class("caption");
        hint.set_hexpand(true);
        hint.set_halign(gtk::Align::End);
        let footer = gtk::Box::new(gtk::Orientation::Horizontal, 6);
        footer.set_margin_start(12);
        footer.set_margin_end(12);
        footer.set_margin_bottom(6);
        footer.append(&clear);
        footer.append(&hint);

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        root.append(&scroller);
        root.append(&footer);

        Rc::new(Self {
            root,
            scroller,
            content,
            pinned_heading,
            pinned_list,
            recent_heading,
            recent_list,
            empty,
            state: RefCell::new(State::default()),
            ctx: RefCell::new(Weak::new()),
        })
    }

    /// Connects the page to the application state; call once.
    pub fn attach(self: &Rc<Self>, ctx: &Rc<AppCtx>) {
        *self.ctx.borrow_mut() = Rc::downgrade(ctx);

        for (list, other) in [
            (&self.pinned_list, &self.recent_list),
            (&self.recent_list, &self.pinned_list),
        ] {
            let page = Rc::downgrade(self);
            let other = other.clone();
            list.connect_row_selected(move |_, row| {
                let (Some(page), Some(row)) = (page.upgrade(), row) else {
                    return;
                };
                other.unselect_all();
                let id = page
                    .state
                    .borrow()
                    .entries
                    .iter()
                    .find(|e| &e.row == row)
                    .map(|e| e.id);
                if let Some(id) = id {
                    page.state.borrow_mut().selected = Some(id);
                }
            });
            let page = Rc::downgrade(self);
            list.connect_row_activated(move |_, row| {
                let Some(page) = page.upgrade() else { return };
                let id = page
                    .state
                    .borrow()
                    .entries
                    .iter()
                    .find(|e| &e.row == row)
                    .map(|e| e.id);
                if let Some(id) = id {
                    page.paste(id, PasteMode::Normal);
                }
            });
        }
    }

    fn ctx(&self) -> Option<Rc<AppCtx>> {
        self.ctx.borrow().upgrade()
    }

    fn paste(&self, id: ItemId, mode: PasteMode) {
        if let Some(ctx) = self.ctx() {
            gtk::glib::spawn_future_local(async move {
                paste_item(&ctx, id, mode).await;
            });
        }
    }

    pub fn set_query(&self, query: &str) {
        self.state.borrow_mut().query = query.to_string();
        self.refresh(false);
    }

    /// Rebuilds the lists. Without `force`, nothing happens unless the set of
    /// visible items changed.
    pub fn refresh(&self, force: bool) {
        let Some(ctx) = self.ctx() else { return };
        let now = now_ms();
        let query = self.state.borrow().query.clone();

        let store = ctx.store.borrow();
        let visible = filter_items(store.items(), &query);
        let pinned: Vec<&ClipItem> = visible.iter().copied().filter(|i| i.pinned).collect();
        let recent: Vec<&ClipItem> = visible
            .iter()
            .copied()
            .filter(|i| !i.pinned)
            .take(MAX_RECENT_ROWS)
            .collect();

        let signature: Vec<(ItemId, bool)> = pinned
            .iter()
            .chain(recent.iter())
            .map(|i| (i.id, i.pinned))
            .collect();
        if !force && self.state.borrow().signature.as_ref() == Some(&signature) {
            return;
        }

        self.pinned_list.remove_all();
        self.recent_list.remove_all();
        let mut entries = Vec::with_capacity(signature.len());
        for (items, list, section) in [
            (&pinned, &self.pinned_list, Section::Pinned),
            (&recent, &self.recent_list, Section::Recent),
        ] {
            for item in items {
                let row = super::row::build_row(item, now);
                list.append(&row);
                entries.push(Entry {
                    id: item.id,
                    section,
                    row: row.upcast(),
                });
            }
        }

        self.pinned_heading.set_visible(!pinned.is_empty());
        self.pinned_list.set_visible(!pinned.is_empty());
        self.recent_heading
            .set_visible(!recent.is_empty() && !pinned.is_empty());
        self.recent_list.set_visible(!recent.is_empty());
        self.empty.set_visible(entries.is_empty());
        if entries.is_empty() {
            self.empty.set_title(if query.trim().is_empty() {
                "Nothing copied yet"
            } else {
                "No results"
            });
        }
        drop(store);

        let keep = self.state.borrow().selected;
        let select = keep
            .filter(|id| entries.iter().any(|e| e.id == *id))
            .or_else(|| entries.first().map(|e| e.id));
        {
            let mut st = self.state.borrow_mut();
            st.entries = entries;
            st.signature = Some(signature);
            st.selected = select;
        }
        self.apply_selection(select, false);
        if force {
            self.scroller.vadjustment().set_value(0.0);
        }
    }

    fn apply_selection(&self, id: Option<ItemId>, scroll: bool) {
        let target = {
            let st = self.state.borrow();
            id.and_then(|id| st.entries.iter().find(|e| e.id == id))
                .map(|e| (e.section, e.row.clone()))
        };
        match target {
            Some((section, row)) => {
                let (list, other) = match section {
                    Section::Pinned => (&self.pinned_list, &self.recent_list),
                    Section::Recent => (&self.recent_list, &self.pinned_list),
                };
                other.unselect_all();
                list.select_row(Some(&row));
                if scroll {
                    self.scroll_to(&row);
                }
            }
            None => {
                self.pinned_list.unselect_all();
                self.recent_list.unselect_all();
            }
        }
    }

    fn scroll_to(&self, row: &gtk::ListBoxRow) {
        let adj = self.scroller.vadjustment();
        let Some(p) = row.compute_point(&self.content, &gtk::graphene::Point::new(0.0, 0.0)) else {
            return;
        };
        let (top, height) = (f64::from(p.y()), f64::from(row.height()));
        let (view_top, view_h) = (adj.value(), adj.page_size());
        if top < view_top {
            adj.set_value((top - 12.0).max(0.0));
        } else if top + height > view_top + view_h {
            adj.set_value(top + height - view_h + 12.0);
        }
    }

    pub fn move_selection(&self, delta: i32) {
        let next = {
            let st = self.state.borrow();
            if st.entries.is_empty() {
                return;
            }
            let current = st
                .selected
                .and_then(|id| st.entries.iter().position(|e| e.id == id));
            let idx = match current {
                Some(i) => (i as i32 + delta).clamp(0, st.entries.len() as i32 - 1) as usize,
                None => 0,
            };
            st.entries[idx].id
        };
        self.state.borrow_mut().selected = Some(next);
        self.apply_selection(Some(next), true);
    }

    pub fn activate_selected(&self, mode: PasteMode) {
        let id = self.state.borrow().selected;
        if let Some(id) = id {
            self.paste(id, mode);
        }
    }

    pub fn toggle_pin_selected(&self) {
        let (Some(ctx), Some(id)) = (self.ctx(), self.state.borrow().selected) else {
            return;
        };
        {
            let mut store = ctx.store.borrow_mut();
            let pinned = store.get(id).is_some_and(|i| i.pinned);
            store.set_pinned(id, !pinned);
        }
        schedule_save(&ctx);
        self.refresh(false);
    }

    pub fn delete_selected(&self) {
        let (Some(ctx), Some(id)) = (self.ctx(), self.state.borrow().selected) else {
            return;
        };
        // Land on the neighbour after the removed row.
        let neighbour = {
            let st = self.state.borrow();
            let idx = st.entries.iter().position(|e| e.id == id);
            idx.and_then(|i| {
                st.entries
                    .get(i + 1)
                    .or_else(|| i.checked_sub(1).and_then(|p| st.entries.get(p)))
            })
            .map(|e| e.id)
        };
        ctx.store.borrow_mut().remove(id);
        schedule_save(&ctx);
        self.state.borrow_mut().selected = neighbour;
        self.refresh(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::TextBody;

    fn text_item(id: ItemId, text: &str) -> ClipItem {
        ClipItem {
            id,
            content: ClipContent::Text {
                text: TextBody::Inline(text.into()),
                html: None,
            },
            created_ms: 0,
            used_ms: 0,
            pinned: false,
            source: None,
        }
    }

    fn image_item(id: ItemId) -> ClipItem {
        ClipItem {
            id,
            content: ClipContent::Image {
                blob: "b.png".into(),
                width: 10,
                height: 10,
                thumb_png: Vec::new(),
            },
            created_ms: 0,
            used_ms: 0,
            pinned: false,
            source: None,
        }
    }

    fn ids(v: Vec<&ClipItem>) -> Vec<ItemId> {
        v.into_iter().map(|i| i.id).collect()
    }

    #[test]
    fn filter_matches_all_terms_case_insensitively() {
        let items = vec![
            text_item(1, "Hello brave World"),
            text_item(2, "hello there"),
            text_item(3, "unrelated"),
        ];
        assert_eq!(ids(filter_items(&items, "hello")), vec![1, 2]);
        assert_eq!(ids(filter_items(&items, "WORLD hello")), vec![1]);
        assert_eq!(ids(filter_items(&items, "  ")), vec![1, 2, 3]);
        assert_eq!(ids(filter_items(&items, "")), vec![1, 2, 3]);
        assert!(filter_items(&items, "hello zebra").is_empty());
    }

    #[test]
    fn filter_matches_images_on_the_word_image() {
        let items = vec![text_item(1, "a picture"), image_item(2)];
        assert_eq!(ids(filter_items(&items, "image")), vec![2]);
        assert_eq!(ids(filter_items(&items, "IMAGE")), vec![2]);
    }

    #[test]
    fn relative_time_table() {
        let now = 1_000_000_000_000;
        let ago = |secs: i64| relative_time(now, now - secs * 1000);
        assert_eq!(ago(0), "just now");
        assert_eq!(ago(59), "just now");
        assert_eq!(ago(61), "1 min ago");
        assert_eq!(ago(2 * 3600), "2 h ago");
        assert_eq!(ago(26 * 3600), "yesterday");
        assert_eq!(ago(3 * 86400), "3 days ago");
    }

    #[test]
    fn relative_time_future_is_just_now() {
        assert_eq!(relative_time(1000, 5000), "just now");
    }
}
