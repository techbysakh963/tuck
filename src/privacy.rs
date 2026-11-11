//! Decides which clipboard contents must never be recorded.

use regex::{Regex, RegexBuilder};

/// Offered by password managers alongside the secret itself.
const PASSWORD_HINT_MIME: &str = "x-kde-passwordManagerHint";

/// Keeps user patterns from compiling into something that stalls the capture path.
const PATTERN_SIZE_LIMIT: usize = 256 * 1024;

pub struct PrivacyFilter {
    patterns: Vec<Regex>,
}

impl PrivacyFilter {
    /// Invalid patterns are logged and skipped so one typo does not disable the rest.
    pub fn new(patterns: &[String]) -> Self {
        let patterns = patterns
            .iter()
            .filter_map(
                |p| match RegexBuilder::new(p).size_limit(PATTERN_SIZE_LIMIT).build() {
                    Ok(re) => Some(re),
                    Err(e) => {
                        log::warn!("ignoring invalid exclusion pattern {p:?}: {e}");
                        None
                    }
                },
            )
            .collect();
        Self { patterns }
    }

    /// True when the source marked this copy as a secret.
    pub fn is_secret_offer(&self, mime_types: &[String], hint_value: Option<&str>) -> bool {
        mime_types
            .iter()
            .any(|m| m.eq_ignore_ascii_case(PASSWORD_HINT_MIME))
            && hint_value.is_some_and(|v| v.trim() == "secret")
    }

    pub fn matches_text(&self, text: &str) -> bool {
        self.patterns.iter().any(|re| re.is_match(text))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mimes(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn password_manager_hint_marks_secret() {
        let f = PrivacyFilter::new(&[]);
        let offered = mimes(&["text/plain", "x-kde-passwordManagerHint"]);
        assert!(f.is_secret_offer(&offered, Some("secret")));
        assert!(!f.is_secret_offer(&offered, Some("other")));
        assert!(!f.is_secret_offer(&offered, None));
    }

    #[test]
    fn absent_hint_is_not_secret() {
        let f = PrivacyFilter::new(&[]);
        assert!(!f.is_secret_offer(&mimes(&["text/plain"]), None));
        // A value without the matching offered type is not trusted.
        assert!(!f.is_secret_offer(&mimes(&["text/plain"]), Some("secret")));
    }

    #[test]
    fn patterns_match_keys_and_not_prose() {
        let f = PrivacyFilter::new(&["^sk-[A-Za-z0-9]{20,}$".to_string()]);
        assert!(f.matches_text("sk-abcdefghijklmnopqrstuvwx"));
        assert!(!f.matches_text("Please skip this sentence"));
    }

    #[test]
    fn invalid_pattern_is_skipped() {
        let f = PrivacyFilter::new(&["[unclosed".to_string(), "ok".to_string()]);
        assert!(f.matches_text("this is ok"));
        assert!(!f.matches_text("nothing"));
    }
}
