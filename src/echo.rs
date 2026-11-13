// SPDX-License-Identifier: GPL-3.0-or-later
//! Recognises clipboard changes that Tuck itself caused.
//!
//! When Tuck puts an item on the clipboard to paste it, the watcher sees that
//! change like any other. Matching it here keeps the history order stable and
//! avoids a duplicate, while a later copy of the same content still counts.

use crate::blobs::hash_bytes;
use crate::capture::Captured;

/// How long after a write the same content is still treated as ours.
pub const ECHO_WINDOW_MS: i64 = 3000;

#[derive(Default)]
pub struct EchoGuard {
    armed: Vec<(u64, i64)>,
}

impl EchoGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Call right before writing `fingerprint`'s content to the clipboard.
    pub fn arm(&mut self, fingerprint: u64, now_ms: i64) {
        self.expire(now_ms);
        self.armed.push((fingerprint, now_ms));
    }

    /// True once per `arm`, and only within the window. Matching is exact.
    pub fn is_echo(&mut self, fingerprint: u64, now_ms: i64) -> bool {
        self.expire(now_ms);
        match self.armed.iter().position(|(f, _)| *f == fingerprint) {
            Some(pos) => {
                self.armed.remove(pos);
                true
            }
            None => false,
        }
    }

    fn expire(&mut self, now_ms: i64) {
        self.armed.retain(|(_, at)| now_ms - at <= ECHO_WINDOW_MS);
    }
}

/// Identifies content independent of its rich-text companion, so the text a
/// paste wrote matches the text the watcher reads back.
pub fn fingerprint(c: &Captured) -> u64 {
    match c {
        Captured::Text { text, .. } => hash_bytes(&[b"t:".as_slice(), text.as_bytes()].concat()),
        Captured::Image { png } => hash_bytes(&[b"i:".as_slice(), png].concat()),
        Captured::Files(paths) => {
            let mut buf = b"f:".to_vec();
            for p in paths {
                buf.extend_from_slice(p.to_string_lossy().as_bytes());
                buf.push(0);
            }
            hash_bytes(&buf)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn text(s: &str) -> Captured {
        Captured::Text {
            text: s.into(),
            html: None,
        }
    }

    #[test]
    fn echo_guard_matches_exactly_within_window() {
        let mut g = EchoGuard::new();
        g.arm(fingerprint(&text("hello")), 1000);
        assert!(g.is_echo(fingerprint(&text("hello")), 1500));
        // Consumed: a later genuine copy of the same text is recorded.
        assert!(!g.is_echo(fingerprint(&text("hello")), 1600));
    }

    #[test]
    fn echo_guard_expires() {
        let mut g = EchoGuard::new();
        g.arm(fingerprint(&text("hello")), 1000);
        assert!(!g.is_echo(fingerprint(&text("hello")), 1000 + ECHO_WINDOW_MS + 1));
    }

    #[test]
    fn echo_guard_does_not_match_superstring() {
        let mut g = EchoGuard::new();
        g.arm(fingerprint(&text("hello")), 1000);
        assert!(!g.is_echo(fingerprint(&text("say hello")), 1100));
        assert!(g.is_echo(fingerprint(&text("hello")), 1200));
    }

    #[test]
    fn html_companion_does_not_change_fingerprint() {
        let plain = text("x");
        let rich = Captured::Text {
            text: "x".into(),
            html: Some("<b>x</b>".into()),
        };
        assert_eq!(fingerprint(&plain), fingerprint(&rich));
    }
}
