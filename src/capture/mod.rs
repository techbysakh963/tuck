//! Clipboard capture: what a watcher reports to the rest of the app.

use std::path::PathBuf;

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
