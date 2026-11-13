// SPDX-License-Identifier: GPL-3.0-or-later
//! The clipboard history index: ordering, de-duplication, pinning, caps and persistence.

use std::collections::HashSet;
use std::fs;
use std::io::{self, Cursor};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use image::imageops::FilterType;
use image::{ImageFormat, ImageReader, Limits};
use serde::{Deserialize, Serialize};

use crate::blobs::{blob_name, BlobStore};
use crate::model::{ClipContent, ClipItem, ItemId, TextBody};

pub const INLINE_TEXT_MAX: usize = 64 * 1024;
pub const PREVIEW_CHARS: usize = 200;
pub const THUMB_MAX_H: u32 = 128;
/// Wider images are scaled too, so a panorama strip does not become a huge thumbnail.
pub const THUMB_MAX_W: u32 = 512;
/// Upper bound on the encoded thumbnail kept in the index.
pub const THUMB_MAX_BYTES: usize = 48 * 1024;
/// Largest side of an image that is decoded at all.
pub const IMAGE_MAX_SIDE: u32 = 16384;
/// Largest memory the PNG decoder may allocate.
pub const IMAGE_MAX_ALLOC: u64 = 256 * 1024 * 1024;
/// Rich-text flavours above this size are dropped; the plain text is kept.
pub const HTML_INLINE_MAX: usize = 256 * 1024;
/// Default total size of all blob files before the oldest unpinned items are evicted.
pub const DEFAULT_MAX_BLOB_BYTES: u64 = 256 * 1024 * 1024;

const INDEX_FILE: &str = "history.json";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddOutcome {
    Added(ItemId),
    Moved(ItemId),
}

impl AddOutcome {
    pub fn id(self) -> ItemId {
        match self {
            AddOutcome::Added(id) | AddOutcome::Moved(id) => id,
        }
    }
}

#[derive(Debug, Default)]
pub struct LoadReport {
    /// Set when the index could not be read and was set aside under this name.
    pub recovered_from_corrupt: Option<PathBuf>,
    /// Set when the index was written by a newer version; it is left untouched
    /// and nothing is saved during this session.
    pub newer_version: Option<u32>,
    /// Set when the data or blob directory could not be created.
    pub dir_unavailable: bool,
}

/// Format version written to `history.json`. Files without a version are version 1.
const INDEX_VERSION: u32 = 1;
/// Version written once the history holds a copied-files item, which older
/// versions cannot read; they then leave the file alone instead of resetting it.
const FILES_INDEX_VERSION: u32 = 2;
/// Newest format this version can read.
const MAX_INDEX_VERSION: u32 = FILES_INDEX_VERSION;

fn default_version() -> u32 {
    INDEX_VERSION
}

#[derive(Serialize, Deserialize)]
struct IndexFile {
    #[serde(default = "default_version")]
    version: u32,
    next_id: ItemId,
    items: Vec<ClipItem>,
}

impl Default for IndexFile {
    fn default() -> Self {
        Self {
            version: INDEX_VERSION,
            next_id: 0,
            items: Vec::new(),
        }
    }
}

/// Reads only the version, so a newer file is recognised even when its items do not parse.
#[derive(Deserialize)]
struct VersionProbe {
    #[serde(default = "default_version")]
    version: u32,
}

/// Error category and position only: serde messages can quote file content.
fn describe(e: &serde_json::Error) -> String {
    format!(
        "{:?} at line {}, column {}",
        e.classify(),
        e.line(),
        e.column()
    )
}

pub struct HistoryStore {
    dir: PathBuf,
    blobs: BlobStore,
    items: Vec<ClipItem>,
    next_id: ItemId,
    max_items: usize,
    max_blob_bytes: u64,
    dirty: bool,
    read_only: bool,
    /// Blobs being written on another thread for items not added yet.
    in_flight: HashSet<String>,
    #[cfg(test)]
    save_count: usize,
}

fn preview_of(text: &str) -> String {
    text.chars().take(PREVIEW_CHARS).collect()
}

fn invalid(msg: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, msg.into())
}

impl HistoryStore {
    /// Opens the store in `dir`. An unreadable index is moved aside, never overwritten.
    pub fn open(dir: &Path, max_items: usize) -> (Self, LoadReport) {
        let mut report = LoadReport::default();
        if let Err(e) = crate::private::create_dir(dir) {
            log::error!("cannot create data directory {}: {e}", dir.display());
            report.dir_unavailable = true;
        }
        let blob_dir = dir.join("blobs");
        let blobs = BlobStore::open(&blob_dir).unwrap_or_else(|e| {
            log::error!("cannot create blob directory {}: {e}", blob_dir.display());
            report.dir_unavailable = true;
            BlobStore::at(&blob_dir)
        });
        let index_path = dir.join(INDEX_FILE);
        let index = match fs::read(&index_path) {
            Ok(bytes) => match serde_json::from_slice::<IndexFile>(&bytes) {
                Ok(index) => index,
                Err(e) => match serde_json::from_slice::<VersionProbe>(&bytes) {
                    Ok(probe) if probe.version > MAX_INDEX_VERSION => {
                        log::warn!(
                            "history index has format version {}, newer than {MAX_INDEX_VERSION}; leaving it untouched",
                            probe.version
                        );
                        report.newer_version = Some(probe.version);
                        IndexFile::default()
                    }
                    _ => {
                        log::error!("history index is corrupt: {}", describe(&e));
                        report.recovered_from_corrupt = set_aside(&index_path);
                        IndexFile::default()
                    }
                },
            },
            Err(e) if e.kind() == io::ErrorKind::NotFound => IndexFile::default(),
            Err(e) => {
                log::error!("cannot read history index: {e}");
                report.recovered_from_corrupt = set_aside(&index_path);
                IndexFile::default()
            }
        };
        if index.version > MAX_INDEX_VERSION {
            report.newer_version = Some(index.version);
            log::warn!(
                "history index has format version {}, newer than {MAX_INDEX_VERSION}; leaving it untouched",
                index.version
            );
        }
        let next_id = index
            .items
            .iter()
            .map(|i| i.id + 1)
            .max()
            .unwrap_or(1)
            .max(index.next_id)
            .max(1);
        let mut store = Self {
            dir: dir.to_path_buf(),
            blobs,
            items: index.items,
            next_id,
            max_items: max_items.max(1),
            max_blob_bytes: DEFAULT_MAX_BLOB_BYTES,
            dirty: false,
            read_only: report.newer_version.is_some(),
            in_flight: HashSet::new(),
            #[cfg(test)]
            save_count: 0,
        };
        store.sort();
        store.enforce_cap();
        store.dirty = false;
        (store, report)
    }

    pub fn add_text(
        &mut self,
        text: String,
        html: Option<String>,
        source: Option<String>,
        now_ms: i64,
    ) -> io::Result<AddOutcome> {
        // Held in memory and rewritten on every save, so only small HTML stays inline.
        let html = html.filter(|h| h.len() <= HTML_INLINE_MAX);
        let body = if text.len() > INLINE_TEXT_MAX {
            let name = self.blobs.put(text.as_bytes(), "txt")?;
            TextBody::Blob {
                name,
                preview: preview_of(&text),
                len: text.len(),
            }
        } else {
            TextBody::Inline(text)
        };
        let existing = self.items.iter().position(|i| match &i.content {
            ClipContent::Text { text, .. } => text == &body,
            _ => false,
        });
        if let Some(pos) = existing {
            let item = &mut self.items[pos];
            item.used_ms = now_ms;
            if let (ClipContent::Text { html: slot, .. }, Some(h)) = (&mut item.content, html) {
                *slot = Some(h);
            }
            let id = item.id;
            self.changed();
            return Ok(AddOutcome::Moved(id));
        }
        let id = self.new_id();
        self.items.push(ClipItem {
            id,
            content: ClipContent::Text { text: body, html },
            created_ms: now_ms,
            used_ms: now_ms,
            pinned: false,
            source,
        });
        self.changed();
        Ok(AddOutcome::Added(id))
    }

    /// Records a list of copied files, given as `file://` URIs.
    pub fn add_files(
        &mut self,
        uris: Vec<String>,
        source: Option<String>,
        now_ms: i64,
    ) -> AddOutcome {
        let existing = self.items.iter().position(|i| match &i.content {
            ClipContent::Files { uris: u } => u == &uris,
            _ => false,
        });
        if let Some(pos) = existing {
            let id = self.items[pos].id;
            self.items[pos].used_ms = now_ms;
            self.changed();
            return AddOutcome::Moved(id);
        }
        let id = self.new_id();
        self.items.push(ClipItem {
            id,
            content: ClipContent::Files { uris },
            created_ms: now_ms,
            used_ms: now_ms,
            pinned: false,
            source,
        });
        self.changed();
        AddOutcome::Added(id)
    }

    pub fn add_image(
        &mut self,
        png: &[u8],
        source: Option<String>,
        now_ms: i64,
    ) -> io::Result<AddOutcome> {
        if let Some(id) = self.touch_image(&blob_name(png, "png"), now_ms) {
            return Ok(AddOutcome::Moved(id));
        }
        let prepared = prepare_image(png, &self.blobs)?;
        Ok(self.add_prepared_image(prepared, source, now_ms))
    }

    /// Marks the item holding this image blob as just used; `None` when the
    /// image is not in the history.
    pub fn touch_image(&mut self, blob_name: &str, now_ms: i64) -> Option<ItemId> {
        let pos = self.items.iter().position(|i| match &i.content {
            ClipContent::Image { blob, .. } => blob == blob_name,
            _ => false,
        })?;
        let id = self.items[pos].id;
        self.items[pos].used_ms = self.items[pos].used_ms.max(now_ms);
        self.changed();
        Some(id)
    }

    /// Whether an item refers to this image blob.
    pub fn has_image_blob(&self, name: &str) -> bool {
        self.items.iter().any(|i| match &i.content {
            ClipContent::Image { blob, .. } => blob == name,
            _ => false,
        })
    }

    /// Where image blobs are written, for preparing an image on another thread.
    pub fn blob_store(&self) -> BlobStore {
        self.blobs.clone()
    }

    /// Keeps a blob, and the temporary file it is written through, from being
    /// pruned while it is prepared for an item that is not in the index yet.
    pub fn hold_blob(&mut self, name: &str) {
        self.in_flight.insert(name.to_string());
    }

    /// Ends `hold_blob`, once the item was added or given up.
    pub fn release_blob(&mut self, name: &str) {
        self.in_flight.remove(name);
    }

    /// Adds an image whose blob and thumbnail `prepare_image` already produced.
    pub fn add_prepared_image(
        &mut self,
        prepared: PreparedImage,
        source: Option<String>,
        now_ms: i64,
    ) -> AddOutcome {
        let existing = self.items.iter().position(|i| match &i.content {
            ClipContent::Image { blob, .. } => blob == &prepared.blob,
            _ => false,
        });
        if let Some(pos) = existing {
            let id = self.items[pos].id;
            self.items[pos].used_ms = self.items[pos].used_ms.max(now_ms);
            self.changed();
            return AddOutcome::Moved(id);
        }
        let id = self.new_id();
        self.items.push(ClipItem {
            id,
            content: ClipContent::Image {
                blob: prepared.blob,
                width: prepared.width,
                height: prepared.height,
                thumb_png: prepared.thumb_png,
            },
            created_ms: now_ms,
            used_ms: now_ms,
            pinned: false,
            source,
        });
        self.changed();
        AddOutcome::Added(id)
    }

    /// Pinned items first, then most recently used first.
    pub fn items(&self) -> &[ClipItem] {
        &self.items
    }

    pub fn get(&self, id: ItemId) -> Option<&ClipItem> {
        self.items.iter().find(|i| i.id == id)
    }

    /// The complete text of an item, reading its blob when the text is large.
    pub fn full_text(&self, id: ItemId) -> io::Result<String> {
        match self.get(id).map(|i| &i.content) {
            Some(ClipContent::Text {
                text: TextBody::Inline(s),
                ..
            }) => Ok(s.clone()),
            Some(ClipContent::Text {
                text: TextBody::Blob { name, .. },
                ..
            }) => Ok(String::from_utf8_lossy(&self.blobs.get(name)?).into_owned()),
            Some(_) => Err(invalid("item is not text")),
            None => Err(io::Error::new(io::ErrorKind::NotFound, "no such item")),
        }
    }

    /// The `file://` URIs of a copied-files item.
    pub fn file_uris(&self, id: ItemId) -> io::Result<Vec<String>> {
        match self.get(id).map(|i| &i.content) {
            Some(ClipContent::Files { uris }) => Ok(uris.clone()),
            Some(_) => Err(invalid("item is not a file list")),
            None => Err(io::Error::new(io::ErrorKind::NotFound, "no such item")),
        }
    }

    /// Bytes of the full-size PNG of an image item.
    pub fn image_png(&self, id: ItemId) -> io::Result<Vec<u8>> {
        match self.get(id).map(|i| &i.content) {
            Some(ClipContent::Image { blob, .. }) => self.blobs.get(blob),
            Some(_) => Err(invalid("item is not an image")),
            None => Err(io::Error::new(io::ErrorKind::NotFound, "no such item")),
        }
    }

    pub fn touch(&mut self, id: ItemId, now_ms: i64) {
        if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
            item.used_ms = now_ms;
            self.changed();
        }
    }

    pub fn set_pinned(&mut self, id: ItemId, pinned: bool) {
        if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
            if item.pinned != pinned {
                item.pinned = pinned;
                self.changed();
            }
        }
    }

    pub fn remove(&mut self, id: ItemId) {
        let before = self.items.len();
        self.items.retain(|i| i.id != id);
        if self.items.len() != before {
            self.dirty = true;
        }
    }

    pub fn clear_unpinned(&mut self) {
        let before = self.items.len();
        self.items.retain(|i| i.pinned);
        if self.items.len() != before {
            self.dirty = true;
        }
    }

    pub fn set_max_blob_bytes(&mut self, bytes: u64) {
        self.max_blob_bytes = bytes;
        self.enforce_cap();
    }

    /// Total size of the blob files the items refer to.
    pub fn blob_bytes(&self) -> u64 {
        self.items.iter().map(|i| self.blob_size(i)).sum()
    }

    fn blob_size(&self, item: &ClipItem) -> u64 {
        match &item.content {
            ClipContent::Text {
                text: TextBody::Blob { name, .. },
                ..
            } => self.blobs.size_of(name),
            ClipContent::Image { blob, .. } => self.blobs.size_of(blob),
            _ => 0,
        }
    }

    pub fn set_max_items(&mut self, n: usize) {
        self.max_items = n.max(1);
        self.enforce_cap();
    }

    /// Drops unpinned items last used more than `max_age_ms` ago.
    pub fn prune_older_than(&mut self, max_age_ms: i64, now_ms: i64) -> usize {
        let cutoff = now_ms - max_age_ms;
        let before = self.items.len();
        self.items.retain(|i| i.pinned || i.used_ms >= cutoff);
        let removed = before - self.items.len();
        if removed > 0 {
            self.dirty = true;
        }
        removed
    }

    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Deletes any `history.corrupt-*.json` copy left by an earlier unreadable index.
    pub fn discard_corrupt_backups(&self) {
        remove_corrupt_copies(&self.dir, None);
    }

    /// Writes the index atomically and deletes blobs no item refers to any more.
    pub fn save(&mut self) -> io::Result<()> {
        // A file from a newer version stays exactly as it is.
        if !self.dirty || self.read_only {
            return Ok(());
        }
        let has_files = self
            .items
            .iter()
            .any(|i| matches!(i.content, ClipContent::Files { .. }));
        let index = IndexFile {
            version: if has_files {
                FILES_INDEX_VERSION
            } else {
                INDEX_VERSION
            },
            next_id: self.next_id,
            items: self.items.clone(),
        };
        let json = serde_json::to_vec(&index).map_err(|e| invalid(e.to_string()))?;
        let tmp = self.dir.join(format!("{INDEX_FILE}.tmp"));
        crate::private::write_file(&tmp, &json)?;
        fs::rename(&tmp, self.dir.join(INDEX_FILE))?;
        self.dirty = false;
        #[cfg(test)]
        {
            self.save_count += 1;
        }
        let mut keep: HashSet<String> = self
            .items
            .iter()
            .filter_map(|i| match &i.content {
                ClipContent::Text {
                    text: TextBody::Blob { name, .. },
                    ..
                } => Some(name.clone()),
                ClipContent::Image { blob, .. } => Some(blob.clone()),
                _ => None,
            })
            .collect();
        for name in &self.in_flight {
            keep.insert(name.clone());
            keep.insert(format!("{name}.tmp"));
        }
        self.blobs.prune(&keep);
        Ok(())
    }

    fn new_id(&mut self) -> ItemId {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn changed(&mut self) {
        self.sort();
        self.enforce_cap();
        self.dirty = true;
    }

    /// Ids grow with insertion, so they break ties between items added in the same millisecond.
    fn sort(&mut self) {
        self.items
            .sort_by_key(|i| std::cmp::Reverse((i.pinned, i.used_ms, i.id)));
    }

    /// Evicts the oldest unpinned items until both the item count and the blob
    /// budget are met; pinned items are never evicted.
    fn enforce_cap(&mut self) {
        while self.items.len() > self.max_items {
            match self.items.iter().rposition(|i| !i.pinned) {
                Some(pos) => {
                    self.items.remove(pos);
                    self.dirty = true;
                }
                None => break,
            }
        }
        let mut total = self.blob_bytes();
        while total > self.max_blob_bytes {
            let Some(pos) = self.items.iter().rposition(|i| !i.pinned) else {
                break;
            };
            total -= self.blob_size(&self.items[pos]);
            self.items.remove(pos);
            self.dirty = true;
        }
    }
}

/// An image decoded, thumbnailed and written to the blob store; ready to be added.
pub struct PreparedImage {
    pub blob: String,
    pub width: u32,
    pub height: u32,
    pub thumb_png: Vec<u8>,
}

/// Decodes `png` under explicit size limits, builds its thumbnail and stores the
/// original. Safe to run on any thread. Untrusted input: a limit or decode error
/// means nothing is stored.
pub fn prepare_image(png: &[u8], blobs: &BlobStore) -> io::Result<PreparedImage> {
    let mut limits = Limits::default();
    limits.max_image_width = Some(IMAGE_MAX_SIDE);
    limits.max_image_height = Some(IMAGE_MAX_SIDE);
    limits.max_alloc = Some(IMAGE_MAX_ALLOC);
    let mut reader = ImageReader::with_format(Cursor::new(png), ImageFormat::Png);
    reader.limits(limits);
    let img = reader
        .decode()
        .map_err(|e| invalid(format!("not a readable PNG within limits: {e}")))?;
    let (width, height) = (img.width(), img.height());
    let thumb_png = make_thumbnail(&img, png)?;
    drop(img);
    let blob = blobs.put(png, "png")?;
    Ok(PreparedImage {
        blob,
        width,
        height,
        thumb_png,
    })
}

/// Fits the image in `THUMB_MAX_W` x `THUMB_MAX_H` and keeps the encoded size under
/// `THUMB_MAX_BYTES`, shrinking further when needed.
fn make_thumbnail(img: &image::DynamicImage, original: &[u8]) -> io::Result<Vec<u8>> {
    let (w, h) = (img.width(), img.height());
    if w <= THUMB_MAX_W && h <= THUMB_MAX_H && original.len() <= THUMB_MAX_BYTES {
        return Ok(original.to_vec());
    }
    let scale = (f64::from(THUMB_MAX_W) / f64::from(w))
        .min(f64::from(THUMB_MAX_H) / f64::from(h))
        .min(1.0);
    let mut tw = ((f64::from(w) * scale).round() as u32).max(1);
    let mut th = ((f64::from(h) * scale).round() as u32).max(1);
    loop {
        let thumb = img.resize_exact(tw, th, FilterType::Triangle);
        let encoded = encode_png(&thumb)?;
        if encoded.len() <= THUMB_MAX_BYTES || (tw <= 8 && th <= 8) {
            return Ok(encoded);
        }
        tw = (tw / 2).max(1);
        th = (th / 2).max(1);
    }
}

fn encode_png(img: &image::DynamicImage) -> io::Result<Vec<u8>> {
    let mut out = Cursor::new(Vec::new());
    img.write_to(&mut out, ImageFormat::Png)
        .map_err(|e| invalid(format!("cannot encode thumbnail: {e}")))?;
    Ok(out.into_inner())
}

/// Renames an unreadable index to `history.corrupt-<unix>.json` so it is never lost.
fn set_aside(path: &Path) -> Option<PathBuf> {
    let secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let backup = path.with_file_name(format!("history.corrupt-{secs}.json"));
    match fs::rename(path, &backup) {
        Ok(()) => {
            if let Some(dir) = backup.parent() {
                remove_corrupt_copies(dir, Some(&backup));
            }
            Some(backup)
        }
        Err(e) => {
            log::error!("cannot back up corrupt history: {e}");
            None
        }
    }
}

/// Removes `history.corrupt-*` files in `dir`, except `keep`.
fn remove_corrupt_copies(dir: &Path, keep: Option<&Path>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with("history.corrupt-") && Some(entry.path().as_path()) != keep {
            let _ = fs::remove_file(entry.path());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ClipContent, TextBody};
    use std::fs;

    fn texts(store: &HistoryStore) -> Vec<String> {
        store
            .items()
            .iter()
            .map(|i| match &i.content {
                ClipContent::Text { text, .. } => text.preview().to_string(),
                ClipContent::Image { .. } => "<image>".into(),
                ClipContent::Files { uris } => uris.join("|"),
            })
            .collect()
    }

    fn add(store: &mut HistoryStore, s: &str, now: i64) -> AddOutcome {
        store.add_text(s.to_string(), None, None, now).unwrap()
    }

    fn big_png(w: u32, h: u32) -> Vec<u8> {
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            image::Rgb([(x / 16) as u8, (y / 16) as u8, ((x ^ y) / 32) as u8])
        });
        let mut out = std::io::Cursor::new(Vec::new());
        img.write_to(&mut out, image::ImageFormat::Png).unwrap();
        out.into_inner()
    }

    #[test]
    fn add_then_reload_round_trips() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut s, _) = HistoryStore::open(tmp.path(), 50);
        add(&mut s, "first", 1);
        add(&mut s, "second", 2);
        s.save().unwrap();
        let (s2, report) = HistoryStore::open(tmp.path(), 50);
        assert!(report.recovered_from_corrupt.is_none());
        assert_eq!(texts(&s2), ["second", "first"]);
    }

    #[test]
    fn store_files_and_dirs_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempfile::tempdir().unwrap();
        let dir = tmp.path().join("data");
        let (mut s, _) = HistoryStore::open(&dir, 50);
        add(&mut s, "secret", 1);
        s.save().unwrap();
        let mode = |p: &Path| fs::metadata(p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&dir), 0o700);
        assert_eq!(mode(&dir.join("blobs")), 0o700);
        assert_eq!(mode(&dir.join(INDEX_FILE)), 0o600);
        let blob = BlobStore::open(&dir.join("blobs")).unwrap();
        let name = blob.put(b"payload", "txt").unwrap();
        assert_eq!(mode(&blob.path_of(&name)), 0o600);
    }

    #[test]
    fn duplicate_text_moves_to_top() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut s, _) = HistoryStore::open(tmp.path(), 50);
        add(&mut s, "a", 1);
        add(&mut s, "b", 2);
        let out = add(&mut s, "a", 3);
        assert!(matches!(out, AddOutcome::Moved(_)));
        assert_eq!(texts(&s), ["a", "b"]);
    }

    #[test]
    fn pinned_items_stay_first_and_survive_clear() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut s, _) = HistoryStore::open(tmp.path(), 50);
        let a = add(&mut s, "a", 1).id();
        add(&mut s, "b", 2);
        s.set_pinned(a, true);
        add(&mut s, "c", 3);
        assert_eq!(texts(&s), ["a", "c", "b"]);
        s.clear_unpinned();
        assert_eq!(texts(&s), ["a"]);
    }

    #[test]
    fn cap_evicts_oldest_unpinned_and_its_blob() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut s, _) = HistoryStore::open(tmp.path(), 3);
        let big = "x".repeat(INLINE_TEXT_MAX + 10);
        add(&mut s, &big, 1);
        add(&mut s, "b", 2);
        add(&mut s, "c", 3);
        s.save().unwrap();
        let blob_dir = tmp.path().join("blobs");
        assert_eq!(fs::read_dir(&blob_dir).unwrap().count(), 1);
        add(&mut s, "d", 4);
        assert_eq!(texts(&s), ["d", "c", "b"]);
        s.save().unwrap();
        assert_eq!(fs::read_dir(&blob_dir).unwrap().count(), 0);
    }

    #[test]
    fn saving_keeps_a_blob_that_is_still_being_prepared() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut s, _) = HistoryStore::open(tmp.path(), 50);
        let blobs = s.blob_store();
        let name = blob_name(b"not added yet", "png");
        let partial = blobs.path_of(&format!("{name}.tmp"));
        s.hold_blob(&name);
        fs::write(&partial, b"half").unwrap();
        add(&mut s, "a", 1);
        s.save().unwrap();
        assert!(partial.exists(), "a blob being written must survive a save");
        assert_eq!(blobs.put(b"not added yet", "png").unwrap(), name);
        add(&mut s, "b", 2);
        s.save().unwrap();
        assert!(blobs.get(&name).is_ok());
        s.release_blob(&name);
        add(&mut s, "c", 3);
        s.save().unwrap();
        assert!(blobs.get(&name).is_err(), "an abandoned blob is pruned");
    }

    #[test]
    fn large_text_is_stored_as_blob() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut s, _) = HistoryStore::open(tmp.path(), 50);
        let big = "é".repeat(100 * 1024 / 2);
        let id = add(&mut s, &big, 1).id();
        s.save().unwrap();
        match &s.get(id).unwrap().content {
            ClipContent::Text {
                text: TextBody::Blob { preview, .. },
                ..
            } => assert_eq!(preview.chars().count(), PREVIEW_CHARS),
            other => panic!("expected blob text, got {other:?}"),
        }
        assert_eq!(s.full_text(id).unwrap(), big);
        let size = fs::metadata(tmp.path().join("history.json")).unwrap().len();
        assert!(size < 4 * 1024, "history.json is {size} bytes");
    }

    #[test]
    fn large_text_dedupes_by_content() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut s, _) = HistoryStore::open(tmp.path(), 50);
        let big = "y".repeat(INLINE_TEXT_MAX + 1);
        add(&mut s, &big, 1);
        add(&mut s, "small", 2);
        assert!(matches!(add(&mut s, &big, 3), AddOutcome::Moved(_)));
        assert_eq!(s.items().len(), 2);
    }

    #[test]
    fn image_keeps_only_thumbnail_in_index() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut s, _) = HistoryStore::open(tmp.path(), 50);
        let png = big_png(4000, 3000);
        let id = s.add_image(&png, None, 1).unwrap().id();
        s.save().unwrap();
        match &s.get(id).unwrap().content {
            ClipContent::Image {
                blob,
                width,
                height,
                thumb_png,
            } => {
                assert_eq!((*width, *height), (4000, 3000));
                let thumb = image::load_from_memory(thumb_png).unwrap();
                assert!(thumb.height() <= THUMB_MAX_H);
                let stored = fs::read(tmp.path().join("blobs").join(blob)).unwrap();
                assert_eq!(stored, png);
            }
            other => panic!("expected image, got {other:?}"),
        }
        let size = fs::metadata(tmp.path().join("history.json")).unwrap().len();
        assert!(size < 64 * 1024, "history.json is {size} bytes");
        let (s2, _) = HistoryStore::open(tmp.path(), 50);
        assert_eq!(s2.items().len(), 1);
    }

    #[test]
    fn burst_of_adds_respects_cap_and_order() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut s, _) = HistoryStore::open(tmp.path(), 50);
        for i in 1..=100 {
            // Same millisecond for all: order must still follow insertion.
            add(&mut s, &i.to_string(), 1000);
        }
        assert_eq!(s.items().len(), 50);
        assert_eq!(texts(&s)[0], "100");
        assert_eq!(texts(&s)[49], "51");
        assert!(s.is_dirty());
        s.save().unwrap();
        assert!(!s.is_dirty());
        assert_eq!(s.save_count, 1);
        s.save().unwrap();
        assert_eq!(s.save_count, 1);
    }

    #[test]
    fn corrupt_history_is_backed_up_and_reset() {
        let tmp = tempfile::tempdir().unwrap();
        fs::write(tmp.path().join("history.json"), b"{not json").unwrap();
        let (s, report) = HistoryStore::open(tmp.path(), 50);
        assert!(s.items().is_empty());
        let backup = report.recovered_from_corrupt.expect("backup path");
        assert_eq!(fs::read(&backup).unwrap(), b"{not json");
        assert!(backup
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("history.corrupt-"));
    }

    #[test]
    fn prune_older_than_keeps_pinned() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut s, _) = HistoryStore::open(tmp.path(), 50);
        let old = add(&mut s, "old pinned", 1).id();
        add(&mut s, "old", 2);
        add(&mut s, "new", 1000);
        s.set_pinned(old, true);
        assert_eq!(s.prune_older_than(500, 1000), 1);
        assert_eq!(texts(&s), ["old pinned", "new"]);
    }

    fn write_index(dir: &Path, json: &str) {
        fs::write(dir.join(INDEX_FILE), json).unwrap();
    }

    #[test]
    fn index_without_version_loads_as_version_one() {
        let tmp = tempfile::tempdir().unwrap();
        write_index(
            tmp.path(),
            r#"{"next_id":5,"items":[{"id":4,"content":{"Text":{"text":{"Inline":"old"},"html":null}},"created_ms":1,"used_ms":1,"pinned":false,"source":null}]}"#,
        );
        let (mut s, report) = HistoryStore::open(tmp.path(), 50);
        assert!(report.newer_version.is_none() && report.recovered_from_corrupt.is_none());
        assert_eq!(texts(&s), ["old"]);
        add(&mut s, "new", 2);
        s.save().unwrap();
        let saved = fs::read_to_string(tmp.path().join(INDEX_FILE)).unwrap();
        assert!(saved.contains("\"version\":1"), "{saved}");
    }

    #[test]
    fn file_items_round_trip_and_mark_the_index_version_two() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut s, _) = HistoryStore::open(tmp.path(), 50);
        add(&mut s, "plain", 1);
        let uris = vec!["file:///home/u/a%20b.txt".to_string()];
        let id = s.add_files(uris.clone(), None, 2).id();
        // The same list again moves the item instead of adding another.
        assert!(matches!(s.add_files(uris.clone(), None, 3), AddOutcome::Moved(i) if i == id));
        s.save().unwrap();
        let saved = fs::read_to_string(tmp.path().join(INDEX_FILE)).unwrap();
        assert!(saved.contains("\"version\":2"), "{saved}");
        let (s2, report) = HistoryStore::open(tmp.path(), 50);
        assert!(report.newer_version.is_none() && report.recovered_from_corrupt.is_none());
        assert_eq!(s2.file_uris(id).unwrap(), uris);
        assert_eq!(s2.items().len(), 2);
    }

    #[test]
    fn history_without_files_keeps_writing_version_one() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut s, _) = HistoryStore::open(tmp.path(), 50);
        add(&mut s, "plain", 1);
        s.save().unwrap();
        let saved = fs::read_to_string(tmp.path().join(INDEX_FILE)).unwrap();
        assert!(saved.contains("\"version\":1"), "{saved}");
    }

    #[test]
    fn index_with_version_one_loads() {
        let tmp = tempfile::tempdir().unwrap();
        write_index(tmp.path(), r#"{"version":1,"next_id":1,"items":[]}"#);
        let (_, report) = HistoryStore::open(tmp.path(), 50);
        assert!(report.newer_version.is_none() && report.recovered_from_corrupt.is_none());
    }

    #[test]
    fn newer_index_is_left_untouched_and_never_saved_over() {
        let tmp = tempfile::tempdir().unwrap();
        let original = r#"{"version":99,"next_id":1,"items":[],"layout":"new"}"#;
        write_index(tmp.path(), original);
        let (mut s, report) = HistoryStore::open(tmp.path(), 50);
        assert_eq!(report.newer_version, Some(99));
        assert!(report.recovered_from_corrupt.is_none());
        add(&mut s, "x", 1);
        s.save().unwrap();
        assert_eq!(
            fs::read_to_string(tmp.path().join(INDEX_FILE)).unwrap(),
            original
        );
        assert_eq!(fs::read_dir(tmp.path()).unwrap().count(), 2); // index and blobs
    }

    #[test]
    fn newer_index_with_unreadable_items_is_still_not_treated_as_corrupt() {
        let tmp = tempfile::tempdir().unwrap();
        let original = r#"{"version":99,"entries":"different shape"}"#;
        write_index(tmp.path(), original);
        let (_, report) = HistoryStore::open(tmp.path(), 50);
        assert_eq!(report.newer_version, Some(99));
        assert_eq!(
            fs::read_to_string(tmp.path().join(INDEX_FILE)).unwrap(),
            original
        );
    }

    #[test]
    fn only_newest_corrupt_copy_is_kept_and_clear_removes_it() {
        let tmp = tempfile::tempdir().unwrap();
        let stale = tmp.path().join("history.corrupt-1.json");
        fs::write(&stale, b"old").unwrap();
        write_index(tmp.path(), "{not json");
        let (s, report) = HistoryStore::open(tmp.path(), 50);
        let backup = report.recovered_from_corrupt.unwrap();
        assert!(backup.exists() && !stale.exists());
        s.discard_corrupt_backups();
        assert!(!backup.exists());
    }

    #[test]
    fn unavailable_data_dir_is_reported() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("occupied");
        fs::write(&file, b"x").unwrap();
        let (_, report) = HistoryStore::open(&file.join("data"), 50);
        assert!(report.dir_unavailable);
    }

    #[test]
    fn parse_error_description_does_not_quote_content() {
        let err = serde_json::from_str::<IndexFile>(r#"{"next_id":"hunter2-secret","items":[]}"#)
            .err()
            .unwrap();
        assert!(err.to_string().contains("hunter2"), "premise: serde quotes");
        assert!(!describe(&err).contains("hunter2"));
    }

    #[test]
    fn blob_budget_evicts_oldest_unpinned() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut s, _) = HistoryStore::open(tmp.path(), 50);
        let big = |c: &str| c.repeat(INLINE_TEXT_MAX + 1000);
        s.set_max_blob_bytes(3 * (INLINE_TEXT_MAX as u64 + 1000));
        let a = add(&mut s, &big("a"), 1).id();
        add(&mut s, &big("b"), 2);
        add(&mut s, &big("c"), 3);
        s.set_pinned(a, true);
        add(&mut s, &big("d"), 4);
        // "b" is the oldest unpinned; the pinned "a" stays despite being oldest.
        let kept: Vec<_> = s.items().iter().map(|i| i.id).collect();
        assert_eq!(kept.len(), 3);
        assert!(kept.contains(&a));
        assert!(s.blob_bytes() <= 3 * (INLINE_TEXT_MAX as u64 + 1000));
        s.save().unwrap();
        assert_eq!(fs::read_dir(tmp.path().join("blobs")).unwrap().count(), 3);
    }

    #[test]
    fn blob_budget_never_evicts_pinned() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut s, _) = HistoryStore::open(tmp.path(), 50);
        let id = add(&mut s, &"p".repeat(INLINE_TEXT_MAX + 1), 1).id();
        s.set_pinned(id, true);
        s.set_max_blob_bytes(10);
        assert!(s.get(id).is_some());
        let other = add(&mut s, &"q".repeat(INLINE_TEXT_MAX + 1), 2).id();
        assert!(
            s.get(other).is_none(),
            "unpinned item over budget is dropped"
        );
    }

    #[test]
    fn large_html_is_dropped_and_small_html_kept() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut s, _) = HistoryStore::open(tmp.path(), 50);
        let big_html = "<p>x</p>".repeat(HTML_INLINE_MAX / 8 + 10);
        let a = s
            .add_text("big".into(), Some(big_html), None, 1)
            .unwrap()
            .id();
        let b = s
            .add_text("small".into(), Some("<b>s</b>".into()), None, 2)
            .unwrap()
            .id();
        let html = |id| match &s.get(id).unwrap().content {
            ClipContent::Text { html, .. } => html.clone(),
            _ => unreachable!(),
        };
        assert_eq!(html(a), None);
        assert_eq!(html(b).as_deref(), Some("<b>s</b>"));
        s.save().unwrap();
        let size = fs::metadata(tmp.path().join("history.json")).unwrap().len();
        assert!(size < 4 * 1024, "history.json is {size} bytes");
    }

    #[test]
    fn very_wide_image_gets_a_small_thumbnail() {
        let tmp = tempfile::tempdir().unwrap();
        let (mut s, _) = HistoryStore::open(tmp.path(), 50);
        let png = big_png(16000, 100);
        let id = s.add_image(&png, None, 1).unwrap().id();
        match &s.get(id).unwrap().content {
            ClipContent::Image {
                width,
                height,
                thumb_png,
                ..
            } => {
                assert_eq!((*width, *height), (16000, 100));
                assert!(
                    thumb_png.len() <= THUMB_MAX_BYTES,
                    "{} bytes",
                    thumb_png.len()
                );
                let thumb = image::load_from_memory(thumb_png).unwrap();
                assert!(thumb.width() <= THUMB_MAX_W && thumb.height() <= THUMB_MAX_H);
            }
            other => panic!("expected image, got {other:?}"),
        }
    }

    #[test]
    fn thumbnail_byte_size_is_capped_for_noisy_images() {
        let tmp = tempfile::tempdir().unwrap();
        let blobs = BlobStore::open(&tmp.path().join("b")).unwrap();
        let mut state = 12345u32;
        let img = image::RgbImage::from_fn(512, 128, |_, _| {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            image::Rgb([(state >> 8) as u8, (state >> 16) as u8, (state >> 24) as u8])
        });
        let mut out = Cursor::new(Vec::new());
        img.write_to(&mut out, ImageFormat::Png).unwrap();
        let p = prepare_image(&out.into_inner(), &blobs).unwrap();
        assert!(p.thumb_png.len() <= THUMB_MAX_BYTES);
    }

    fn crc32(data: &[u8]) -> u32 {
        let mut crc = 0xffff_ffffu32;
        for &b in data {
            crc ^= u32::from(b);
            for _ in 0..8 {
                crc = if crc & 1 != 0 {
                    (crc >> 1) ^ 0xedb8_8320
                } else {
                    crc >> 1
                };
            }
        }
        !crc
    }

    #[test]
    fn oversized_declared_dimensions_are_rejected_cheaply() {
        let tmp = tempfile::tempdir().unwrap();
        let blobs = BlobStore::open(&tmp.path().join("b")).unwrap();
        let mut png = big_png(8, 8);
        // IHDR data starts at byte 16: width, height. Declare 40000 x 40000.
        png[16..20].copy_from_slice(&40000u32.to_be_bytes());
        png[20..24].copy_from_slice(&40000u32.to_be_bytes());
        let crc = crc32(&png[12..29]);
        png[29..33].copy_from_slice(&crc.to_be_bytes());
        assert!(png.len() < 1024);
        let started = std::time::Instant::now();
        let err = prepare_image(&png, &blobs).err().expect("must be rejected");
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
        assert!(started.elapsed() < std::time::Duration::from_secs(2));
        assert_eq!(fs::read_dir(tmp.path().join("b")).unwrap().count(), 0);
    }
}
