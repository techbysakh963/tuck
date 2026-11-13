// SPDX-License-Identifier: GPL-3.0-or-later
//! Copied files: how a list is stored and which flavours are offered on paste.

use std::path::{Path, PathBuf};

/// `file://` URI for a local path, percent-encoding everything unsafe.
pub fn file_uri(path: &Path) -> String {
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

/// The paths a stored URI list stands for, as the watcher would read them.
pub fn paths_of(uris: &[String]) -> Vec<PathBuf> {
    crate::capture::parse_uri_list(&uris.join("\n"))
}

/// Last path component, or the whole path when it has none (such as `/`).
fn name_of(path: &Path) -> String {
    path.file_name()
        .unwrap_or(path.as_os_str())
        .to_string_lossy()
        .into_owned()
}

/// Row title for a file list: the names, comma separated.
pub fn display_names(uris: &[String]) -> String {
    paths_of(uris)
        .iter()
        .map(|p| name_of(p))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The flavours offered when a file list is pasted.
#[derive(Debug, PartialEq, Eq)]
pub struct Payloads {
    /// `text/uri-list`: CRLF separated.
    pub uri_list: String,
    /// `x-special/gnome-copied-files`: the action, then one URI per line.
    pub gnome_copied: String,
    /// Plain text fallback: the paths, one per line.
    pub plain: String,
}

pub fn payloads(uris: &[String]) -> Payloads {
    let paths = paths_of(uris);
    Payloads {
        uri_list: uris.iter().map(|u| format!("{u}\r\n")).collect(),
        gnome_copied: format!("copy\n{}", uris.join("\n")),
        plain: paths
            .iter()
            .map(|p| p.to_string_lossy())
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn uris() -> Vec<String> {
        [
            Path::new("/home/u/My Notes.txt"),
            Path::new("/home/u/b.txt"),
        ]
        .iter()
        .map(|p| file_uri(p))
        .collect()
    }

    #[test]
    fn uris_round_trip_to_paths() {
        assert_eq!(
            paths_of(&uris()),
            vec![
                PathBuf::from("/home/u/My Notes.txt"),
                PathBuf::from("/home/u/b.txt")
            ]
        );
    }

    #[test]
    fn names_are_listed() {
        assert_eq!(display_names(&uris()), "My Notes.txt, b.txt");
    }

    #[test]
    fn payloads_cover_every_consumer() {
        let p = payloads(&uris());
        assert_eq!(
            p.uri_list,
            "file:///home/u/My%20Notes.txt\r\nfile:///home/u/b.txt\r\n"
        );
        assert_eq!(
            p.gnome_copied,
            "copy\nfile:///home/u/My%20Notes.txt\nfile:///home/u/b.txt"
        );
        assert_eq!(p.plain, "/home/u/My Notes.txt\n/home/u/b.txt");
    }

    #[test]
    fn offered_payload_reads_back_as_the_same_files() {
        // What the watcher parses from our own uri-list must equal what we stored.
        let p = payloads(&uris());
        assert_eq!(
            crate::capture::parse_uri_list(&p.uri_list),
            paths_of(&uris())
        );
    }
}
