//! Tuck: clipboard history for GNOME.

pub mod blobs;
pub mod capture;
pub mod echo;
pub mod history;
pub mod ingest;
pub mod kind;
pub mod logging;
pub mod model;
pub mod privacy;
pub mod private;
pub mod settings;

/// Application identifier shared by the desktop entry, D-Bus name and settings.
pub const APP_ID: &str = "io.github.techbysakh963.Tuck";
