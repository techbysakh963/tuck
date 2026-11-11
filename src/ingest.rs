//! Turns capture events into history entries, applying the privacy and echo rules.

use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};

use crate::capture::{CaptureEvent, Captured};
use crate::echo::{fingerprint, EchoGuard};
use crate::history::{AddOutcome, HistoryStore};
use crate::privacy::PrivacyFilter;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SkipReason {
    Empty,
    Paused,
    Secret,
    Excluded,
    Echo,
}

#[derive(Debug, PartialEq, Eq)]
pub enum IngestResult {
    Stored(AddOutcome),
    Skipped(SkipReason),
}

const PNG_MAGIC: &[u8] = b"\x89PNG\r\n\x1a\n";

/// Largest image file that is read into memory when a file copy is turned into an image entry.
const MAX_IMAGE_FILE_BYTES: u64 = 64 * 1024 * 1024;

pub fn ingest(
    ev: CaptureEvent,
    store: &mut HistoryStore,
    filter: &PrivacyFilter,
    echo: &mut EchoGuard,
    paused: bool,
    now_ms: i64,
) -> io::Result<IngestResult> {
    if paused {
        return Ok(IngestResult::Skipped(SkipReason::Paused));
    }
    if filter.is_secret_offer(&ev.mime_types, ev.secret_hint.as_deref()) {
        return Ok(IngestResult::Skipped(SkipReason::Secret));
    }
    if is_empty(&ev.captured) {
        return Ok(IngestResult::Skipped(SkipReason::Empty));
    }
    if let Captured::Text { text, .. } = &ev.captured {
        if filter.matches_text(text) {
            return Ok(IngestResult::Skipped(SkipReason::Excluded));
        }
    }
    if echo.is_echo(fingerprint(&ev.captured), now_ms) {
        return Ok(IngestResult::Skipped(SkipReason::Echo));
    }

    let outcome = match ev.captured {
        Captured::Text { text, html } => store.add_text(text, html, ev.source, now_ms)?,
        Captured::Image { png } => store.add_image(&png, ev.source, now_ms)?,
        Captured::Files(paths) => {
            if let Some(png) = single_png_file(&paths) {
                match store.add_image(&png, ev.source.clone(), now_ms) {
                    Ok(outcome) => return Ok(IngestResult::Stored(outcome)),
                    Err(e) if e.kind() == io::ErrorKind::InvalidData => {}
                    Err(e) => return Err(e),
                }
            }
            let text = paths
                .iter()
                .map(|p| file_uri(p))
                .collect::<Vec<_>>()
                .join("\n");
            store.add_text(text, None, ev.source, now_ms)?
        }
    };
    Ok(IngestResult::Stored(outcome))
}

fn is_empty(c: &Captured) -> bool {
    match c {
        Captured::Text { text, .. } => text.trim().is_empty(),
        Captured::Image { png } => png.is_empty(),
        Captured::Files(paths) => paths.is_empty(),
    }
}

/// The file's bytes when the copy is exactly one existing PNG file.
fn single_png_file(paths: &[PathBuf]) -> Option<Vec<u8>> {
    let [path] = paths else { return None };
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() || meta.len() > MAX_IMAGE_FILE_BYTES {
        return None;
    }
    let mut head = [0u8; 8];
    fs::File::open(path).ok()?.read_exact(&mut head).ok()?;
    if head != PNG_MAGIC {
        return None;
    }
    fs::read(path).ok()
}

fn file_uri(path: &Path) -> String {
    let mut out = String::from("file://");
    for b in path.to_string_lossy().bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'/' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::ClipContent;

    struct Fixture {
        _tmp: tempfile::TempDir,
        store: HistoryStore,
        echo: EchoGuard,
        filter: PrivacyFilter,
    }

    fn fixture(exclusions: &[&str]) -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let (store, _) = HistoryStore::open(&tmp.path().join("data"), 50);
        let patterns: Vec<String> = exclusions.iter().map(|s| s.to_string()).collect();
        Fixture {
            _tmp: tmp,
            store,
            echo: EchoGuard::new(),
            filter: PrivacyFilter::new(&patterns),
        }
    }

    fn text_event(s: &str) -> CaptureEvent {
        CaptureEvent {
            captured: Captured::Text {
                text: s.into(),
                html: None,
            },
            mime_types: vec!["text/plain".into()],
            secret_hint: None,
            source: None,
        }
    }

    fn run(f: &mut Fixture, ev: CaptureEvent, paused: bool, now: i64) -> IngestResult {
        ingest(ev, &mut f.store, &f.filter, &mut f.echo, paused, now).unwrap()
    }

    #[test]
    fn empty_or_whitespace_capture_is_ignored() {
        let mut f = fixture(&[]);
        for s in ["", "   ", "\n\t "] {
            assert_eq!(
                run(&mut f, text_event(s), false, 1),
                IngestResult::Skipped(SkipReason::Empty)
            );
        }
        let no_files = CaptureEvent {
            captured: Captured::Files(vec![]),
            ..text_event("")
        };
        assert_eq!(
            run(&mut f, no_files, false, 1),
            IngestResult::Skipped(SkipReason::Empty)
        );
        assert!(f.store.items().is_empty());
    }

    #[test]
    fn paused_skips() {
        let mut f = fixture(&[]);
        assert_eq!(
            run(&mut f, text_event("hi"), true, 1),
            IngestResult::Skipped(SkipReason::Paused)
        );
        assert!(f.store.items().is_empty());
    }

    #[test]
    fn secret_hint_skips() {
        let mut f = fixture(&[]);
        let mut ev = text_event("hunter2");
        ev.mime_types.push("x-kde-passwordManagerHint".into());
        ev.secret_hint = Some("secret".into());
        assert_eq!(
            run(&mut f, ev, false, 1),
            IngestResult::Skipped(SkipReason::Secret)
        );
        assert!(f.store.items().is_empty());
    }

    #[test]
    fn excluded_text_skips() {
        let mut f = fixture(&["^sk-[A-Za-z0-9]{20,}$"]);
        assert_eq!(
            run(&mut f, text_event("sk-abcdefghijklmnopqrstuvwx"), false, 1),
            IngestResult::Skipped(SkipReason::Excluded)
        );
        assert!(matches!(
            run(&mut f, text_event("fine"), false, 2),
            IngestResult::Stored(_)
        ));
    }

    #[test]
    fn own_paste_is_skipped_once_then_recorded() {
        let mut f = fixture(&[]);
        let ev = text_event("pasted");
        f.echo.arm(fingerprint(&ev.captured), 1000);
        assert_eq!(
            run(&mut f, ev.clone(), false, 1100),
            IngestResult::Skipped(SkipReason::Echo)
        );
        assert!(matches!(
            run(&mut f, ev, false, 1200),
            IngestResult::Stored(AddOutcome::Added(_))
        ));
    }

    #[test]
    fn copied_image_file_becomes_image() {
        let mut f = fixture(&[]);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("shot.png");
        image::RgbImage::new(8, 6).save(&path).unwrap();
        let ev = CaptureEvent {
            captured: Captured::Files(vec![path]),
            ..text_event("")
        };
        let IngestResult::Stored(out) = run(&mut f, ev, false, 1) else {
            panic!("expected stored");
        };
        let item = f.store.get(out.id()).unwrap();
        assert!(matches!(
            item.content,
            ClipContent::Image {
                width: 8,
                height: 6,
                ..
            }
        ));
    }

    #[test]
    fn other_file_lists_store_file_uris() {
        let mut f = fixture(&[]);
        let ev = CaptureEvent {
            captured: Captured::Files(vec![
                PathBuf::from("/home/u/My Notes.txt"),
                PathBuf::from("/home/u/b.txt"),
            ]),
            ..text_event("")
        };
        let IngestResult::Stored(out) = run(&mut f, ev, false, 1) else {
            panic!("expected stored");
        };
        assert_eq!(
            f.store.full_text(out.id()).unwrap(),
            "file:///home/u/My%20Notes.txt\nfile:///home/u/b.txt"
        );
    }
}
