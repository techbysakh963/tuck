//! A virtualised grid of glyphs, shared by the Emoji and Symbols tabs.

use std::cell::{Cell, OnceCell, RefCell};
use std::rc::{Rc, Weak};

use gtk4 as gtk;
use gtk4::glib;
use gtk4::pango;
use gtk4::prelude::*;
use libadwaita as adw;

use super::paste_flow::insert_glyph;
use super::symbols::{search_entries, GridEntry};
use super::AppCtx;

const COLUMNS: u32 = 8;
const RECENT_LIMIT: usize = 24;

pub struct GridPage {
    pub root: gtk::Box,
    grid: gtk::GridView,
    model: gtk::StringList,
    recent_heading: gtk::Label,
    recent: gtk::FlowBox,
    empty: adw::StatusPage,
    entries: Rc<OnceCell<Vec<GridEntry>>>,
    load_entries: fn() -> Vec<GridEntry>,
    /// The model is filled the first time the page is shown, not at startup.
    built: Cell<bool>,
    with_recent: bool,
    query: RefCell<String>,
    ctx: RefCell<Weak<AppCtx>>,
}

fn glyph_label() -> gtk::Label {
    let label = gtk::Label::new(None);
    // Scaled through Pango, not styling: the grid has to show glyphs large
    // enough to recognise.
    let attrs = pango::AttrList::new();
    attrs.insert(pango::AttrFloat::new_scale(1.6));
    label.set_attributes(Some(&attrs));
    label.set_margin_top(6);
    label.set_margin_bottom(6);
    label
}

impl GridPage {
    pub fn new(load_entries: fn() -> Vec<GridEntry>, with_recent: bool) -> Rc<Self> {
        let entries: Rc<OnceCell<Vec<GridEntry>>> = Rc::default();
        let model = gtk::StringList::new(&[]);

        let factory = gtk::SignalListItemFactory::new();
        factory.connect_setup(|_, item| {
            if let Some(item) = item.downcast_ref::<gtk::ListItem>() {
                item.set_child(Some(&glyph_label()));
            }
        });
        let table = entries.clone();
        factory.connect_bind(move |_, item| {
            let Some(item) = item.downcast_ref::<gtk::ListItem>() else {
                return;
            };
            let Some(label) = item.child().and_downcast::<gtk::Label>() else {
                return;
            };
            let index = item
                .item()
                .and_downcast::<gtk::StringObject>()
                .and_then(|s| s.string().parse::<usize>().ok());
            if let Some(entry) = index.and_then(|i| table.get()?.get(i)) {
                label.set_text(&entry.glyph);
                label.set_tooltip_text(Some(&entry.name));
            }
        });

        let selection = gtk::SingleSelection::new(Some(model.clone()));
        selection.set_autoselect(false);
        selection.set_can_unselect(true);
        let grid = gtk::GridView::new(Some(selection), Some(factory));
        grid.set_min_columns(COLUMNS);
        grid.set_max_columns(COLUMNS);
        grid.set_single_click_activate(true);
        grid.set_vexpand(true);

        let scroller = gtk::ScrolledWindow::builder()
            .child(&grid)
            .vexpand(true)
            .hscrollbar_policy(gtk::PolicyType::Never)
            .build();
        scroller.set_margin_start(6);
        scroller.set_margin_end(6);

        let recent_heading = gtk::Label::new(Some("Recent"));
        recent_heading.add_css_class("heading");
        recent_heading.set_xalign(0.0);
        recent_heading.set_margin_start(12);
        recent_heading.set_margin_top(6);
        let recent = gtk::FlowBox::new();
        recent.set_max_children_per_line(COLUMNS);
        recent.set_min_children_per_line(COLUMNS);
        recent.set_selection_mode(gtk::SelectionMode::None);
        recent.set_activate_on_single_click(true);
        recent.set_margin_start(6);
        recent.set_margin_end(6);
        recent.set_homogeneous(true);

        let empty = adw::StatusPage::builder()
            .icon_name("edit-find-symbolic")
            .title("No results")
            .vexpand(true)
            .visible(false)
            .build();

        let root = gtk::Box::new(gtk::Orientation::Vertical, 0);
        if with_recent {
            root.append(&recent_heading);
            root.append(&recent);
        }
        root.append(&scroller);
        root.append(&empty);

        Rc::new(Self {
            root,
            grid,
            model,
            recent_heading,
            recent,
            empty,
            entries,
            load_entries,
            built: Cell::new(false),
            with_recent,
            query: RefCell::new(String::new()),
            ctx: RefCell::new(Weak::new()),
        })
    }

    pub fn attach(self: &Rc<Self>, ctx: &Rc<AppCtx>) {
        *self.ctx.borrow_mut() = Rc::downgrade(ctx);

        let page = Rc::downgrade(self);
        self.grid.connect_activate(move |_, position| {
            let Some(page) = page.upgrade() else { return };
            let glyph = page
                .model
                .string(position)
                .and_then(|s| s.parse::<usize>().ok())
                .and_then(|i| page.entries().get(i))
                .map(|e| e.glyph.clone());
            if let Some(glyph) = glyph {
                page.insert(&glyph);
            }
        });

        let page = Rc::downgrade(self);
        self.recent.connect_child_activated(move |_, child| {
            let Some(page) = page.upgrade() else { return };
            let glyph = child
                .child()
                .and_downcast::<gtk::Label>()
                .map(|l| l.text().to_string());
            if let Some(glyph) = glyph {
                page.insert(&glyph);
            }
        });
    }

    fn entries(&self) -> &[GridEntry] {
        self.entries.get_or_init(self.load_entries)
    }

    /// Fills the grid on first use; later calls do nothing.
    pub fn ensure_built(&self) {
        if self.built.replace(true) {
            return;
        }
        self.show_matches();
        self.refresh_recent();
    }

    fn insert(&self, glyph: &str) {
        let Some(ctx) = self.ctx.borrow().upgrade() else {
            return;
        };
        if self.with_recent {
            let mut settings = ctx.settings.borrow_mut();
            settings.recent_emoji.retain(|g| g != glyph);
            settings.recent_emoji.insert(0, glyph.to_string());
            settings.recent_emoji.truncate(RECENT_LIMIT);
            if let Err(e) = settings.save(&ctx.config_path) {
                log::error!("cannot save settings: {e}");
            }
        }
        let glyph = glyph.to_string();
        glib::spawn_future_local(async move {
            insert_glyph(&ctx, &glyph).await;
        });
    }

    pub fn set_query(&self, query: &str) {
        if *self.query.borrow() == query {
            return;
        }
        *self.query.borrow_mut() = query.to_string();
        if self.built.get() {
            self.show_matches();
            self.refresh_recent();
        }
    }

    fn show_matches(&self) {
        let matches = search_entries(self.entries(), &self.query.borrow());
        let strings: Vec<String> = matches.iter().map(usize::to_string).collect();
        let refs: Vec<&str> = strings.iter().map(String::as_str).collect();
        self.model.splice(0, self.model.n_items(), &refs);
        self.empty.set_visible(matches.is_empty());
        if let Some(scroller) = self.grid.parent() {
            scroller.set_visible(!matches.is_empty());
        }
    }

    /// Rebuilds the strip of recently used emoji; it only shows while the
    /// search box is empty.
    pub fn refresh_recent(&self) {
        if !self.with_recent {
            return;
        }
        let glyphs: Vec<String> = self
            .ctx
            .borrow()
            .upgrade()
            .map(|c| c.settings.borrow().recent_emoji.clone())
            .unwrap_or_default();
        self.recent.remove_all();
        for glyph in &glyphs {
            let label = glyph_label();
            label.set_text(glyph);
            self.recent.append(&label);
        }
        let show = !glyphs.is_empty() && self.query.borrow().trim().is_empty();
        self.recent.set_visible(show);
        self.recent_heading.set_visible(show);
    }

    /// Scrolls back to the top and forgets the previous search.
    pub fn reset(&self) {
        self.set_query("");
        if self.built.get() && self.model.n_items() > 0 {
            self.grid
                .scroll_to(0, gtk::ListScrollFlags::NONE, None::<gtk::ScrollInfo>);
        }
        self.refresh_recent();
    }

    pub fn focus_grid(&self) {
        self.ensure_built();
        if self.model.n_items() > 0 {
            self.grid
                .scroll_to(0, gtk::ListScrollFlags::FOCUS, None::<gtk::ScrollInfo>);
        }
    }

    /// Enter in the search box inserts the best (first) match.
    pub fn activate_first(&self) {
        self.ensure_built();
        if self.model.n_items() > 0 {
            self.grid.emit_by_name::<()>("activate", &[&0u32]);
        }
    }
}
