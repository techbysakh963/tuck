//! Putting a history item on the clipboard and typing the paste shortcut.

use gtk4::gdk;
use gtk4::glib;
use gtk4::prelude::*;

use std::rc::Rc;

use crate::capture::Captured;
use crate::echo::fingerprint;
use crate::ingest::SkipReason;
use crate::model::{ClipContent, ItemId};
use crate::target::combo_for;

use super::{hide_and_wait_for_target, now_ms, schedule_save, AppCtx};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteMode {
    Normal,
    ForceTerminal,
}

/// Which history item the clipboard holds right now; `None` when it is
/// unknown (a copy Tuck skipped, or something from before Tuck started).
pub type ClipboardHolder = Option<ItemId>;

/// The holder after a capture was skipped. Our own paste echoing back changes
/// nothing; any other skipped copy replaced the clipboard with something that
/// is not in history.
pub fn holder_after_skip(current: ClipboardHolder, reason: SkipReason) -> ClipboardHolder {
    match reason {
        SkipReason::Echo => current,
        _ => None,
    }
}

/// The item to put back after an insert: only one that was on the clipboard
/// before and is still in history.
pub fn restore_choice(previous: ClipboardHolder, existing: &[ItemId]) -> Option<ItemId> {
    previous.filter(|id| existing.contains(id))
}

/// What to put on the clipboard, with the fingerprint the watcher will see.
struct Prepared {
    provider: gdk::ContentProvider,
    fingerprint: u64,
}

fn prepare(ctx: &AppCtx, id: ItemId) -> Result<Prepared, String> {
    let store = ctx.store.borrow();
    let item = store.get(id).ok_or("that item no longer exists")?;
    match &item.content {
        ClipContent::Text { html, .. } => {
            let text = store
                .full_text(id)
                .map_err(|e| format!("cannot read the item: {e}"))?;
            let fp = fingerprint(&Captured::Text {
                text: text.clone(),
                html: None,
            });
            let plain = gdk::ContentProvider::for_value(&text.to_value());
            let provider = match html {
                Some(html) => {
                    let bytes = glib::Bytes::from(html.as_bytes());
                    let rich = gdk::ContentProvider::for_bytes("text/html", &bytes);
                    gdk::ContentProvider::new_union(&[rich, plain])
                }
                None => plain,
            };
            Ok(Prepared {
                provider,
                fingerprint: fp,
            })
        }
        ClipContent::Image { .. } => {
            let png = store
                .image_png(id)
                .map_err(|e| format!("cannot read the image: {e}"))?;
            // The stored PNG is offered as-is: decoding a large screenshot into
            // a texture would cost far more memory, and the watcher then reads
            // back exactly the bytes fingerprinted here.
            let fp = fingerprint(&Captured::Image { png: png.clone() });
            let bytes = glib::Bytes::from_owned(png);
            Ok(Prepared {
                provider: gdk::ContentProvider::for_bytes("image/png", &bytes),
                fingerprint: fp,
            })
        }
    }
}

/// Sets the clipboard, hides the popup, and types the paste shortcut for the
/// target seen when the popup opened.
pub async fn paste_item(ctx: &Rc<AppCtx>, id: ItemId, mode: PasteMode) {
    let started = std::time::Instant::now();
    let prepared = match prepare(ctx, id) {
        Ok(p) => p,
        Err(e) => {
            fail(ctx, &e);
            return;
        }
    };

    // The clipboard must be set while the popup still has focus; Wayland
    // only lets the focused client take the selection.
    let clipboard = ctx.window.window.clipboard();
    ctx.echo.borrow_mut().arm(prepared.fingerprint, now_ms());
    if let Err(e) = clipboard.set_content(Some(&prepared.provider)) {
        fail(ctx, &format!("cannot set the clipboard: {e}"));
        return;
    }
    ctx.clipboard_holder.set(Some(id));

    hide_and_wait_for_target(ctx).await;

    let combo = combo_for(ctx.target.get(), mode == PasteMode::ForceTerminal);
    let sent = ctx.keys.borrow_mut().send(combo);
    if let Err(e) = sent {
        fail(ctx, &format!("paste failed: {e}"));
        return;
    }

    ctx.store.borrow_mut().touch(id, now_ms());
    ctx.clipboard_holder.set(Some(id));
    schedule_save(ctx);
    ctx.window.clipboard.refresh(false);
    log::info!(
        "pasted item with {combo:?} in {} ms",
        started.elapsed().as_millis()
    );
}

fn fail(ctx: &AppCtx, message: &str) {
    log::warn!("{message}");
    ctx.queue_toast(message);
}

/// How long the target gets to read an inserted glyph before the previous
/// clipboard contents return.
const RESTORE_DELAY: std::time::Duration = std::time::Duration::from_millis(400);

/// Types a single emoji or symbol into the target without it becoming a
/// history entry, then puts the previous clipboard item back.
pub async fn insert_glyph(ctx: &Rc<AppCtx>, glyph: &str) {
    let clipboard = ctx.window.window.clipboard();
    let previous = ctx.clipboard_holder.get();
    let captured = Captured::Text {
        text: glyph.to_string(),
        html: None,
    };
    ctx.echo.borrow_mut().arm(fingerprint(&captured), now_ms());
    let provider = gdk::ContentProvider::for_value(&glyph.to_value());
    if let Err(e) = clipboard.set_content(Some(&provider)) {
        fail(ctx, &format!("cannot set the clipboard: {e}"));
        return;
    }
    ctx.clipboard_holder.set(None);

    hide_and_wait_for_target(ctx).await;

    let combo = combo_for(ctx.target.get(), false);
    if let Err(e) = ctx.keys.borrow_mut().send(combo) {
        fail(ctx, &format!("insert failed: {e}"));
        return;
    }

    glib::timeout_future(RESTORE_DELAY).await;
    // Something else copied in the meantime: leave the newer content alone.
    if clipboard.content().as_ref() != Some(&provider) {
        log::info!("clipboard changed during the insert; not restoring");
        return;
    }
    restore_previous(ctx, previous);
}

/// Puts the item that was on the clipboard before the insert back, arming the
/// echo guard so the watcher does not count it as a new copy. When that item
/// is unknown the glyph stays: restoring some other item would be wrong.
fn restore_previous(ctx: &AppCtx, previous: ClipboardHolder) {
    let ids: Vec<ItemId> = ctx.store.borrow().items().iter().map(|i| i.id).collect();
    let Some(id) = restore_choice(previous, &ids) else {
        log::info!("previous clipboard content is not in history; not restoring");
        return;
    };
    match prepare(ctx, id) {
        Ok(p) => {
            ctx.echo.borrow_mut().arm(p.fingerprint, now_ms());
            if let Err(e) = ctx.window.window.clipboard().set_content(Some(&p.provider)) {
                log::warn!("cannot restore the clipboard: {e}");
            } else {
                ctx.clipboard_holder.set(Some(id));
            }
        }
        Err(e) => log::warn!("cannot restore the clipboard: {e}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_echo_keeps_the_holder() {
        assert_eq!(holder_after_skip(Some(3), SkipReason::Echo), Some(3));
        for r in [
            SkipReason::Secret,
            SkipReason::Excluded,
            SkipReason::Paused,
            SkipReason::Empty,
        ] {
            assert_eq!(holder_after_skip(Some(3), r), None);
        }
        assert_eq!(holder_after_skip(None, SkipReason::Echo), None);
    }

    #[test]
    fn restore_needs_a_known_item_that_still_exists() {
        assert_eq!(restore_choice(Some(2), &[1, 2, 3]), Some(2));
        assert_eq!(restore_choice(Some(9), &[1, 2, 3]), None);
        assert_eq!(restore_choice(None, &[1, 2, 3]), None);
    }
}
