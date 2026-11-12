//! One history entry as a list row.

use gtk4 as gtk;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::kind::{classify, Kind};
use crate::model::{ClipContent, ClipItem};

use super::clipboard_page::relative_time;

/// Size of the colour swatch and image thumbnail in a row.
const LEADING_SIZE: i32 = 40;

/// The single line shown as a row title.
pub fn item_title(item: &ClipItem) -> String {
    match &item.content {
        ClipContent::Text { text, .. } => one_line(text.head()),
        ClipContent::Image { width, height, .. } => format!("Image, {width} × {height}"),
        ClipContent::Files { uris } => crate::files::display_names(uris),
    }
}

/// Collapses whitespace so multi-line text reads as one line.
fn one_line(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn subtitle(item: &ClipItem, now_ms: i64) -> String {
    let when = relative_time(now_ms, item.used_ms);
    match &item.source {
        Some(source) => format!("{source} · {when}"),
        None => when,
    }
}

pub fn build_row(item: &ClipItem, now_ms: i64) -> adw::ActionRow {
    let title = item_title(item);
    let row = adw::ActionRow::builder()
        .title(gtk::glib::markup_escape_text(&title))
        .subtitle(gtk::glib::markup_escape_text(&subtitle(item, now_ms)))
        .title_lines(1)
        .subtitle_lines(1)
        .activatable(true)
        .build();

    match &item.content {
        ClipContent::Text { text, .. } => {
            let kind = classify(text.preview());
            if matches!(kind, Kind::Code | Kind::Color) {
                row.add_css_class("monospace");
            }
            if kind == Kind::Color {
                if let Some(swatch) = colour_swatch(text.preview().trim()) {
                    row.add_prefix(&swatch);
                }
            }
        }
        ClipContent::Files { .. } => {
            let icon = gtk::Image::from_icon_name("text-x-generic-symbolic");
            icon.add_css_class("dim-label");
            row.add_prefix(&icon);
        }
        ClipContent::Image { thumb_png, .. } => {
            if let Some(picture) = thumbnail(thumb_png) {
                row.add_prefix(&picture);
            }
        }
    }

    if item.pinned {
        let pin = gtk::Image::from_icon_name("view-pin-symbolic");
        pin.add_css_class("dim-label");
        row.add_suffix(&pin);
    }
    row
}

/// Paints the item's own colour value; the only colour Tuck ever draws.
fn colour_swatch(value: &str) -> Option<gtk::DrawingArea> {
    let rgba = gtk::gdk::RGBA::parse(value).ok()?;
    let area = gtk::DrawingArea::builder()
        .content_width(LEADING_SIZE)
        .content_height(LEADING_SIZE / 2)
        .valign(gtk::Align::Center)
        .build();
    area.set_draw_func(move |_, cr, w, h| {
        let (w, h) = (f64::from(w), f64::from(h));
        let r = 6.0_f64.min(h / 2.0);
        cr.new_sub_path();
        cr.arc(w - r, r, r, -std::f64::consts::FRAC_PI_2, 0.0);
        cr.arc(w - r, h - r, r, 0.0, std::f64::consts::FRAC_PI_2);
        cr.arc(
            r,
            h - r,
            r,
            std::f64::consts::FRAC_PI_2,
            std::f64::consts::PI,
        );
        cr.arc(
            r,
            r,
            r,
            std::f64::consts::PI,
            3.0 * std::f64::consts::FRAC_PI_2,
        );
        cr.close_path();
        cr.set_source_rgba(
            f64::from(rgba.red()),
            f64::from(rgba.green()),
            f64::from(rgba.blue()),
            f64::from(rgba.alpha()),
        );
        let _ = cr.fill();
    });
    Some(area)
}

fn thumbnail(png: &[u8]) -> Option<gtk::Picture> {
    let bytes = gtk::glib::Bytes::from(png);
    let texture = gtk::gdk::Texture::from_bytes(&bytes).ok()?;
    let picture = gtk::Picture::for_paintable(&texture);
    picture.set_content_fit(gtk::ContentFit::Contain);
    picture.set_can_shrink(true);
    picture.set_size_request(LEADING_SIZE, LEADING_SIZE);
    picture.set_valign(gtk::Align::Center);
    Some(picture)
}
