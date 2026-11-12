//! Data types for clipboard history entries.

use serde::{Deserialize, Serialize};

/// Monotonic identifier of a history item; never reused.
pub type ItemId = u64;

/// Text kept in the index, or moved to a blob file when it is large.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TextBody {
    Inline(String),
    /// Large text lives in a blob file; the index only keeps a short preview.
    Blob {
        name: String,
        preview: String,
        len: usize,
    },
}

impl TextBody {
    /// Text suitable for showing in a list row.
    pub fn preview(&self) -> &str {
        match self {
            TextBody::Inline(s) => s,
            TextBody::Blob { preview, .. } => preview,
        }
    }

    /// The first `PREVIEW_CHARS` characters of the preview: all a row title or
    /// search needs, however long the stored text is.
    pub fn head(&self) -> &str {
        let text = self.preview();
        match text.char_indices().nth(crate::history::PREVIEW_CHARS) {
            Some((end, _)) => &text[..end],
            None => text,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ClipContent {
    Text {
        text: TextBody,
        html: Option<String>,
    },
    /// Copied files, kept as `file://` URIs so any path survives the JSON index.
    Files { uris: Vec<String> },
    Image {
        /// Blob name of the full-size PNG.
        blob: String,
        width: u32,
        height: u32,
        #[serde(with = "png_bytes")]
        thumb_png: Vec<u8>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClipItem {
    pub id: ItemId,
    pub content: ClipContent,
    pub created_ms: i64,
    pub used_ms: i64,
    pub pinned: bool,
    pub source: Option<String>,
}

/// Thumbnails are stored as base64 so the index stays compact.
mod png_bytes {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine;
    use serde::{Deserialize, Deserializer, Serializer};

    pub fn serialize<S: Serializer>(bytes: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&STANDARD.encode(bytes))
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let text = String::deserialize(d)?;
        STANDARD.decode(text).map_err(serde::de::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::PREVIEW_CHARS;

    #[test]
    fn head_cuts_at_a_character_boundary() {
        let long = TextBody::Inline("é".repeat(PREVIEW_CHARS + 50));
        assert_eq!(long.head().chars().count(), PREVIEW_CHARS);
        let short = TextBody::Inline("abc".into());
        assert_eq!(short.head(), "abc");
    }
}
