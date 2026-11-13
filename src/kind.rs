// SPDX-License-Identifier: GPL-3.0-or-later
//! Guesses what kind of thing a piece of copied text is, for row styling.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Text,
    Code,
    Link,
    Color,
}

/// First words that, on their own line, make a clip read as a shell command.
const SHELL_WORDS: &[&str] = &[
    "git", "cd", "sudo", "ssh", "docker", "cargo", "npm", "kubectl",
];

const URL_SCHEMES: &[&str] = &["http://", "https://", "ftp://", "file://", "mailto:"];

/// Classifies a clip with a small, fixed rule set; the first match wins:
///
/// 1. Link: a single token starting with a well-known URL scheme.
/// 2. Color: `#rgb`, `#rgba`, `#rrggbb`, `#rrggbbaa` or `rgb()/rgba()/hsl()/hsla()`.
/// 3. Code: starts with `$ `, starts with a common shell command followed by
///    arguments, or is multi-line and contains `{` or `;`.
/// 4. Text: everything else.
pub fn classify(text: &str) -> Kind {
    let t = text.trim();
    if t.is_empty() {
        return Kind::Text;
    }
    if is_link(t) {
        Kind::Link
    } else if is_color(t) {
        Kind::Color
    } else if is_code(t) {
        Kind::Code
    } else {
        Kind::Text
    }
}

fn is_link(t: &str) -> bool {
    let lower = t.to_ascii_lowercase();
    !t.chars().any(char::is_whitespace)
        && URL_SCHEMES
            .iter()
            .any(|s| lower.starts_with(s) && lower.len() > s.len())
}

fn is_color(t: &str) -> bool {
    if let Some(hex) = t.strip_prefix('#') {
        return matches!(hex.len(), 3 | 4 | 6 | 8) && hex.chars().all(|c| c.is_ascii_hexdigit());
    }
    let lower = t.to_ascii_lowercase();
    ["rgb(", "rgba(", "hsl(", "hsla("].iter().any(|f| {
        lower.strip_prefix(f).is_some_and(|rest| {
            rest.strip_suffix(')').is_some_and(|args| {
                !args.is_empty()
                    && args
                        .chars()
                        .all(|c| c.is_ascii_digit() || matches!(c, ' ' | ',' | '.' | '%' | '/'))
            })
        })
    })
}

fn is_code(t: &str) -> bool {
    if t.starts_with("$ ") {
        return true;
    }
    let first_line = t.lines().next().unwrap_or("");
    let mut words = first_line.split_whitespace();
    if let (Some(first), Some(_arg)) = (words.next(), words.next()) {
        if SHELL_WORDS.contains(&first) {
            return true;
        }
    }
    t.contains('\n') && (t.contains('{') || t.contains(';'))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_common_clips() {
        let cases = [
            ("https://gtk.org/x", Kind::Link),
            ("#3584e4", Kind::Color),
            ("rgb(53, 132, 228)", Kind::Color),
            ("git commit --amend", Kind::Code),
            ("fn main() {\n}", Kind::Code),
            ("Meeting moved to Thursday", Kind::Text),
            ("#42", Kind::Text),
            ("#hashtag", Kind::Text),
        ];
        for (text, want) in cases {
            assert_eq!(classify(text), want, "{text:?}");
        }
    }

    #[test]
    fn surrounding_whitespace_is_ignored() {
        assert_eq!(classify("  https://gtk.org\n"), Kind::Link);
        assert_eq!(classify("$ ls -la"), Kind::Code);
        assert_eq!(classify("a link: https://gtk.org"), Kind::Text);
    }
}
