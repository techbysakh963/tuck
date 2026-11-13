// SPDX-License-Identifier: GPL-3.0-or-later
//! Clipboard capture: what a watcher reports to the rest of the app.

use std::cell::Cell;
use std::path::PathBuf;
use std::time::Duration;

mod data_control;
mod x11;

/// Largest single clipboard payload a watcher will read (bytes).
pub(crate) const MAX_PAYLOAD: usize = 32 * 1024 * 1024;

/// Logged when a payload is dropped for its size; never includes the content.
pub(crate) fn log_oversized() {
    log::warn!(
        "clipboard content larger than {} MiB skipped",
        MAX_PAYLOAD >> 20
    );
}

/// Hint some password managers attach to their copies.
const SECRET_HINT_MIME: &str = "x-kde-passwordManagerHint";

/// The clipboard payload a watcher decided to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Captured {
    Text { text: String, html: Option<String> },
    Image { png: Vec<u8> },
    Files(Vec<PathBuf>),
}

/// One clipboard change as seen by a capture backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaptureEvent {
    pub captured: Captured,
    /// Every MIME type the owner offered.
    pub mime_types: Vec<String>,
    /// Value of `x-kde-passwordManagerHint`, when offered and readable.
    pub secret_hint: Option<String>,
    /// Best-effort name of the application that owns the selection.
    pub source: Option<String>,
}

/// How clipboard events are obtained in this session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    ExtDataControl,
    WlrDataControl,
    X11,
}

/// What the session offers, gathered without side effects.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionEnv {
    pub wayland_display: bool,
    pub x_display: bool,
    /// Interface names advertised by the Wayland registry.
    pub globals: Vec<String>,
}

/// Picks the best backend: native data-control first, then X11 (which also
/// covers GNOME, where the compositor mirrors its clipboard into XWayland).
pub fn select_backend(env: &SessionEnv) -> Option<BackendKind> {
    if env.wayland_display {
        let has = |name: &str| env.globals.iter().any(|g| g == name);
        if has("ext_data_control_manager_v1") {
            return Some(BackendKind::ExtDataControl);
        }
        if has("zwlr_data_control_manager_v1") {
            return Some(BackendKind::WlrDataControl);
        }
    }
    env.x_display.then_some(BackendKind::X11)
}

/// Inspects the environment and, under Wayland, lists the registry globals.
pub fn probe_session() -> SessionEnv {
    let nonempty = |k: &str| std::env::var_os(k).is_some_and(|v| !v.is_empty());
    let mut env = SessionEnv {
        wayland_display: nonempty("WAYLAND_DISPLAY"),
        x_display: nonempty("DISPLAY"),
        globals: Vec::new(),
    };
    if env.wayland_display {
        match data_control::list_globals() {
            Ok(globals) => env.globals = globals,
            Err(e) => {
                log::warn!("could not list Wayland globals: {e}");
                env.wayland_display = false;
            }
        }
    }
    env
}

/// Whether the watcher is delivering clipboard changes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptureStatus {
    Stopped,
    Recovered,
}

const BACKOFF_START: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);

/// Runs `connect_and_run` until it returns `Ok` (the receiver is gone). After a
/// failure it waits, doubling from one second up to thirty, and tries again.
/// `connect_and_run` calls the function it is given once it is watching, which
/// resets the wait and reports recovery. The wait only happens while broken, so
/// there is no polling while the watcher is healthy.
pub(crate) fn supervise(
    mut connect_and_run: impl FnMut(&dyn Fn()) -> Result<(), String>,
    notify: impl Fn(CaptureStatus),
    mut wait: impl FnMut(Duration),
) {
    let broken = Cell::new(false);
    let backoff = Cell::new(BACKOFF_START);
    let ready = || {
        backoff.set(BACKOFF_START);
        if broken.replace(false) {
            notify(CaptureStatus::Recovered);
        }
    };
    loop {
        match connect_and_run(&ready) {
            Ok(()) => return,
            Err(e) => {
                log::error!("clipboard watcher stopped: {e}");
                if !broken.replace(true) {
                    notify(CaptureStatus::Stopped);
                }
                wait(backoff.get());
                backoff.set((backoff.get() * 2).min(BACKOFF_MAX));
            }
        }
    }
}

/// Starts the watcher on its own thread. It re-probes the session and
/// reconnects whenever the backend fails, and reports status changes on
/// `status`. The thread ends when the event receiver is gone.
pub fn spawn(
    tx: async_channel::Sender<CaptureEvent>,
    status: async_channel::Sender<CaptureStatus>,
) -> std::thread::JoinHandle<()> {
    std::thread::Builder::new()
        .name("tuck-capture".into())
        .spawn(move || {
            supervise(
                |ready| {
                    let env = probe_session();
                    let kind = select_backend(&env)
                        .ok_or_else(|| format!("no clipboard backend available: {env:?}"))?;
                    log::info!("clipboard backend: {kind:?}");
                    match kind {
                        BackendKind::X11 => x11::run(&tx, ready),
                        BackendKind::ExtDataControl => data_control::run_ext(&tx, ready),
                        BackendKind::WlrDataControl => data_control::run_wlr(&tx, ready),
                    }
                },
                |s| {
                    let _ = status.send_blocking(s);
                },
                std::thread::sleep,
            );
        })
        .expect("spawn capture thread")
}

/// The representation worth reading from an offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Preferred {
    Png,
    Html,
    Utf8Text,
    UriList,
}

const TEXT_MIMES: [&str; 4] = [
    "text/plain;charset=utf-8",
    "UTF8_STRING",
    "text/plain",
    "STRING",
];

/// The offered text flavour to read, best first.
pub(crate) fn text_mime(offered: &[String]) -> Option<&'static str> {
    TEXT_MIMES
        .into_iter()
        .find(|m| offered.iter().any(|o| o == m))
}

/// Decides which flavour to read: a file copy, then rich text (with its plain
/// companion), then plain text. An image wins only when no text is offered:
/// spreadsheets offer a picture next to the cell text, which is the better copy.
pub fn choose_target(offered: &[String]) -> Option<Preferred> {
    let has = |m: &str| offered.iter().any(|o| o == m);
    let has_text = text_mime(offered).is_some();
    // File managers also offer the path as text; the marker tells a file copy
    // apart from an application that merely offers a link next to its text.
    let file_copy = has("x-special/gnome-copied-files");
    if has("text/uri-list") && (!has_text || file_copy) {
        return Some(Preferred::UriList);
    }
    if has("text/html") && has_text {
        return Some(Preferred::Html);
    }
    if has_text {
        return Some(Preferred::Utf8Text);
    }
    has("image/png").then_some(Preferred::Png)
}

/// Reads the chosen flavour through `read` (mime type -> bytes) and builds the
/// event body. Returns `None` when nothing usable could be read.
pub(crate) fn read_capture(
    offered: &[String],
    source: Option<String>,
    mut read: impl FnMut(&str) -> Option<Vec<u8>>,
) -> Option<CaptureEvent> {
    let preferred = choose_target(offered)?;
    // Fail closed: when a password-manager hint is offered, read it before any
    // content and drop the event if it cannot be read.
    let secret_hint = match offered
        .iter()
        .find(|o| o.eq_ignore_ascii_case(SECRET_HINT_MIME))
    {
        Some(name) => Some(String::from_utf8_lossy(&read(name)?).into_owned()),
        None => None,
    };
    let captured = match preferred {
        Preferred::Png => Captured::Image {
            png: read("image/png")?,
        },
        Preferred::UriList => {
            let raw = read("text/uri-list")?;
            let files = parse_uri_list(&String::from_utf8_lossy(&raw));
            if files.is_empty() {
                return None;
            }
            Captured::Files(files)
        }
        Preferred::Html | Preferred::Utf8Text => {
            let text = String::from_utf8_lossy(&read(text_mime(offered)?)?).into_owned();
            let html = if preferred == Preferred::Html {
                read("text/html").map(|b| String::from_utf8_lossy(&b).into_owned())
            } else {
                None
            };
            Captured::Text { text, html }
        }
    };
    Some(CaptureEvent {
        captured,
        mime_types: offered.to_vec(),
        secret_hint,
        source,
    })
}

/// Parses a `text/uri-list` body into local paths; other schemes and
/// comments are skipped.
pub fn parse_uri_list(raw: &str) -> Vec<PathBuf> {
    raw.lines()
        .filter_map(|line| {
            let rest = line.trim().strip_prefix("file://")?;
            // Drop an optional host ("file://localhost/x").
            let path = &rest[rest.find('/')?..];
            Some(PathBuf::from(percent_decode(path)))
        })
        .collect()
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes
                .get(i + 1..i + 3)
                .and_then(|h| std::str::from_utf8(h).ok())
                .and_then(|h| u8::from_str_radix(h, 16).ok());
            if let Some(v) = hex {
                out.push(v);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn env(wayland: bool, x: bool, globals: &[&str]) -> SessionEnv {
        SessionEnv {
            wayland_display: wayland,
            x_display: x,
            globals: v(globals),
        }
    }

    #[test]
    fn select_backend_table() {
        let gnome = env(true, true, &["wl_compositor", "wl_seat"]);
        assert_eq!(select_backend(&gnome), Some(BackendKind::X11));
        let kde = env(true, true, &["wl_seat", "ext_data_control_manager_v1"]);
        assert_eq!(select_backend(&kde), Some(BackendKind::ExtDataControl));
        let sway = env(true, false, &["zwlr_data_control_manager_v1"]);
        assert_eq!(select_backend(&sway), Some(BackendKind::WlrDataControl));
        let both = env(
            true,
            true,
            &[
                "zwlr_data_control_manager_v1",
                "ext_data_control_manager_v1",
            ],
        );
        assert_eq!(select_backend(&both), Some(BackendKind::ExtDataControl));
        assert_eq!(
            select_backend(&env(false, true, &[])),
            Some(BackendKind::X11)
        );
        assert_eq!(select_backend(&env(true, false, &["wl_seat"])), None);
        assert_eq!(select_backend(&env(false, false, &[])), None);
    }

    #[test]
    fn supervise_backs_off_then_recovers() {
        let attempts = Cell::new(0);
        let waits = std::cell::RefCell::new(Vec::new());
        let events = std::cell::RefCell::new(Vec::new());
        supervise(
            |ready| {
                attempts.set(attempts.get() + 1);
                if attempts.get() <= 2 {
                    return Err("down".into());
                }
                ready();
                Ok(())
            },
            |s| events.borrow_mut().push(s),
            |d| waits.borrow_mut().push(d),
        );
        assert_eq!(
            *waits.borrow(),
            vec![Duration::from_secs(1), Duration::from_secs(2)]
        );
        assert_eq!(
            *events.borrow(),
            vec![CaptureStatus::Stopped, CaptureStatus::Recovered]
        );
    }

    #[test]
    fn supervise_caps_the_wait_and_resets_after_recovery() {
        let attempts = Cell::new(0);
        let waits = std::cell::RefCell::new(Vec::new());
        supervise(
            |ready| {
                attempts.set(attempts.get() + 1);
                match attempts.get() {
                    1..=7 => Err("down".into()),
                    8 => {
                        ready();
                        Err("dropped again".into())
                    }
                    _ => Ok(()),
                }
            },
            |_| {},
            |d| waits.borrow_mut().push(d.as_secs()),
        );
        assert_eq!(*waits.borrow(), vec![1, 2, 4, 8, 16, 30, 30, 1]);
    }

    #[test]
    fn choose_target_cases() {
        // Firefox: text and html.
        let firefox = v(&["text/html", "text/plain;charset=utf-8", "text/plain"]);
        assert_eq!(choose_target(&firefox), Some(Preferred::Html));
        // Screenshot.
        assert_eq!(
            choose_target(&v(&["image/png", "image/bmp"])),
            Some(Preferred::Png)
        );
        // Browser "Copy image": png with html but no plain text stays an image.
        assert_eq!(
            choose_target(&v(&["image/png", "text/html"])),
            Some(Preferred::Png)
        );
        // Spreadsheet cell: text wins over the picture, html rides along.
        assert_eq!(
            choose_target(&v(&["image/png", "text/html", "text/plain;charset=utf-8"])),
            Some(Preferred::Html)
        );
        assert_eq!(
            choose_target(&v(&["image/png", "UTF8_STRING"])),
            Some(Preferred::Utf8Text)
        );
        // File manager copy, with a text fallback.
        let nautilus = v(&[
            "x-special/gnome-copied-files",
            "text/uri-list",
            "text/plain;charset=utf-8",
        ]);
        assert_eq!(choose_target(&nautilus), Some(Preferred::UriList));
        // Uri list alone.
        assert_eq!(
            choose_target(&v(&["text/uri-list"])),
            Some(Preferred::UriList)
        );
        // A link next to text stays text.
        assert_eq!(
            choose_target(&v(&["text/uri-list", "UTF8_STRING"])),
            Some(Preferred::Utf8Text)
        );
        // Password manager: plain text, hint read separately.
        let keepass = v(&["text/plain", "x-kde-passwordManagerHint"]);
        assert_eq!(choose_target(&keepass), Some(Preferred::Utf8Text));
        // Html without any text is not usable; nothing offered neither.
        assert_eq!(choose_target(&v(&["text/html"])), None);
        assert_eq!(choose_target(&[]), None);
        assert_eq!(choose_target(&v(&["TARGETS", "TIMESTAMP"])), None);
    }

    #[test]
    fn read_capture_reads_hint_separately() {
        let offered = v(&["text/plain", "x-kde-passwordManagerHint"]);
        let event = read_capture(&offered, None, |m| match m {
            "text/plain" => Some(b"hunter2".to_vec()),
            "x-kde-passwordManagerHint" => Some(b"secret".to_vec()),
            _ => None,
        })
        .unwrap();
        assert_eq!(
            event.captured,
            Captured::Text {
                text: "hunter2".into(),
                html: None
            }
        );
        assert_eq!(event.secret_hint.as_deref(), Some("secret"));
    }

    #[test]
    fn read_capture_matches_hint_case_insensitively() {
        let offered = v(&["text/plain", "x-kde-passwordmanagerhint"]);
        let event = read_capture(&offered, None, |m| match m {
            "text/plain" => Some(b"hunter2".to_vec()),
            "x-kde-passwordmanagerhint" => Some(b"secret".to_vec()),
            _ => None,
        })
        .unwrap();
        assert_eq!(event.secret_hint.as_deref(), Some("secret"));
    }

    #[test]
    fn read_capture_drops_event_when_offered_hint_is_unreadable() {
        let offered = v(&["text/plain", "x-kde-passwordManagerHint"]);
        let mut content_read = false;
        let event = read_capture(&offered, None, |m| {
            if m == "text/plain" {
                content_read = true;
                Some(b"hunter2".to_vec())
            } else {
                None
            }
        });
        assert!(event.is_none());
        assert!(!content_read, "content must not be read before the hint");
    }

    #[test]
    fn read_capture_html_keeps_both_flavours() {
        let offered = v(&["text/html", "UTF8_STRING"]);
        let event = read_capture(&offered, Some("firefox".into()), |m| match m {
            "UTF8_STRING" => Some(b"hi".to_vec()),
            "text/html" => Some(b"<b>hi</b>".to_vec()),
            _ => None,
        })
        .unwrap();
        assert_eq!(
            event.captured,
            Captured::Text {
                text: "hi".into(),
                html: Some("<b>hi</b>".into())
            }
        );
        assert_eq!(event.source.as_deref(), Some("firefox"));
    }

    #[test]
    fn read_capture_gives_up_when_read_fails() {
        assert!(read_capture(&v(&["image/png"]), None, |_| None).is_none());
    }

    #[test]
    fn uri_list_is_decoded() {
        let raw =
            "# comment\r\nfile:///home/a/My%20File.txt\r\nhttp://x/y\r\nfile://localhost/tmp/b\r\n";
        assert_eq!(
            parse_uri_list(raw),
            vec![
                PathBuf::from("/home/a/My File.txt"),
                PathBuf::from("/tmp/b")
            ]
        );
    }
}
