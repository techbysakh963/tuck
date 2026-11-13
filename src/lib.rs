// SPDX-License-Identifier: GPL-3.0-or-later
//! Tuck: clipboard history for GNOME.

pub mod app;
pub mod autostart;
pub mod blobs;
pub mod capture;
pub mod echo;
pub mod files;
pub mod history;
pub mod ingest;
pub mod kind;
pub mod logging;
pub mod model;
pub mod paste;
pub mod privacy;
pub mod private;
pub mod settings;
pub mod shortcut;
pub mod target;
pub mod tray;

/// Application identifier shared by the desktop entry, D-Bus name and settings.
pub const APP_ID: &str = "io.github.techbysakh963.Tuck";
