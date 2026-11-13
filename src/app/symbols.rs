// SPDX-License-Identifier: GPL-3.0-or-later
//! Emoji and symbol data for the grid pages, plus the search over it.

use std::sync::OnceLock;

use gtk4::prelude::*;

/// One cell of an emoji or symbol grid.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GridEntry {
    pub glyph: String,
    pub name: String,
    pub keywords: Vec<String>,
}

fn is_skin_tone_modifier(c: char) -> bool {
    ('\u{1F3FB}'..='\u{1F3FF}').contains(&c)
}

/// All emoji for the grid. Skin-tone variants are left out: the grid shows the
/// default form only, which keeps it a third smaller.
pub fn emoji_entries() -> Vec<GridEntry> {
    emojis::iter()
        .filter(|e| !e.as_str().chars().any(is_skin_tone_modifier))
        .map(|e| GridEntry {
            glyph: e.as_str().to_string(),
            name: e.name().to_string(),
            keywords: e.shortcodes().map(|s| s.replace('_', " ")).collect(),
        })
        .collect()
}

/// The emoji the installed fonts can draw. The emoji data follows the latest
/// Unicode, but many systems ship an older emoji font, and a character it lacks
/// would show as an empty box. Needs GTK to be initialised.
pub fn drawable_emoji_entries() -> Vec<GridEntry> {
    let all = emoji_entries();
    let context = gtk4::Label::new(None).pango_context();
    let fontset = context
        .font_description()
        .zip(context.language())
        .and_then(|(desc, language)| context.load_fontset(&desc, &language));
    let Some(fontset) = fontset else {
        return all;
    };
    let drawable: Vec<GridEntry> = all
        .iter()
        .filter(|e| font_can_draw(&fontset, &e.glyph))
        .cloned()
        .collect();
    // With no emoji font at all, showing the boxes beats an empty grid.
    if drawable.len() * 4 < all.len() {
        return all;
    }
    drawable
}

fn font_can_draw(fontset: &gtk4::pango::Fontset, glyph: &str) -> bool {
    glyph
        .chars()
        .next()
        .is_some_and(|c| fontset.font(c as u32).has_char(c))
}

/// Indices of entries matching `query`: every whitespace-separated term must
/// occur, case-insensitively, in the name or one of the keywords. An empty
/// query matches everything.
pub fn search_entries(entries: &[GridEntry], query: &str) -> Vec<usize> {
    let terms: Vec<String> = query.split_whitespace().map(str::to_lowercase).collect();
    entries
        .iter()
        .enumerate()
        .filter(|(_, e)| {
            if terms.is_empty() {
                return true;
            }
            let name = e.name.to_lowercase();
            terms.iter().all(|t| {
                name.contains(t.as_str()) || e.keywords.iter().any(|k| k.to_lowercase().contains(t))
            })
        })
        .map(|(i, _)| i)
        .collect()
}

/// (glyph, name, space-separated keywords)
type Row = (&'static str, &'static str, &'static str);

const SYMBOLS: &[Row] = &[
    // Arrows
    ("→", "rightwards arrow", "arrow right"),
    ("←", "leftwards arrow", "arrow left"),
    ("↑", "upwards arrow", "arrow up"),
    ("↓", "downwards arrow", "arrow down"),
    ("↔", "left right arrow", "arrow horizontal"),
    ("↕", "up down arrow", "arrow vertical"),
    ("↖", "north west arrow", "arrow diagonal"),
    ("↗", "north east arrow", "arrow diagonal"),
    ("↘", "south east arrow", "arrow diagonal"),
    ("↙", "south west arrow", "arrow diagonal"),
    ("⇒", "rightwards double arrow", "arrow right implies"),
    ("⇐", "leftwards double arrow", "arrow left"),
    ("⇔", "left right double arrow", "arrow iff equivalent"),
    ("⇑", "upwards double arrow", "arrow up"),
    ("⇓", "downwards double arrow", "arrow down"),
    ("↩", "leftwards arrow with hook", "arrow return"),
    ("↪", "rightwards arrow with hook", "arrow"),
    ("↺", "anticlockwise open circle arrow", "arrow undo rotate"),
    ("↻", "clockwise open circle arrow", "arrow redo rotate"),
    ("➜", "heavy round-tipped rightwards arrow", "arrow right"),
    ("↦", "rightwards arrow from bar", "arrow maps to"),
    ("⟶", "long rightwards arrow", "arrow right"),
    ("⟵", "long leftwards arrow", "arrow left"),
    ("⟷", "long left right arrow", "arrow"),
    // Math
    ("±", "plus-minus sign", "math plus minus"),
    ("∓", "minus-or-plus sign", "math"),
    ("×", "multiplication sign", "math times multiply"),
    ("÷", "division sign", "math divide"),
    ("−", "minus sign", "math subtract"),
    ("≠", "not equal to", "math inequality"),
    ("≈", "almost equal to", "math approximately"),
    ("≡", "identical to", "math equivalent"),
    ("≤", "less-than or equal to", "math inequality"),
    ("≥", "greater-than or equal to", "math inequality"),
    ("≪", "much less-than", "math"),
    ("≫", "much greater-than", "math"),
    ("∞", "infinity", "math"),
    ("√", "square root", "math radical"),
    ("∛", "cube root", "math radical"),
    ("∑", "n-ary summation", "math sum sigma"),
    ("∏", "n-ary product", "math product"),
    ("∫", "integral", "math calculus"),
    ("∂", "partial differential", "math calculus derivative"),
    ("∇", "nabla", "math gradient del"),
    ("∆", "increment", "math delta change"),
    ("∈", "element of", "math set"),
    ("∉", "not an element of", "math set"),
    ("⊂", "subset of", "math set"),
    ("⊆", "subset of or equal to", "math set"),
    ("∪", "union", "math set"),
    ("∩", "intersection", "math set"),
    ("∅", "empty set", "math set"),
    ("∀", "for all", "math logic universal"),
    ("∃", "there exists", "math logic existential"),
    ("¬", "not sign", "logic negation"),
    ("∧", "logical and", "logic"),
    ("∨", "logical or", "logic"),
    ("∴", "therefore", "math logic"),
    ("∵", "because", "math logic"),
    ("∝", "proportional to", "math"),
    ("∠", "angle", "math geometry"),
    ("⊥", "up tack", "math perpendicular"),
    ("∥", "parallel to", "math geometry"),
    ("°", "degree sign", "temperature angle"),
    ("′", "prime", "math minutes feet"),
    ("″", "double prime", "math seconds inches"),
    ("‰", "per mille sign", "percent thousand"),
    ("π", "pi", "math constant"),
    ("ℏ", "reduced planck constant", "physics"),
    ("ℝ", "double-struck capital r", "math real numbers"),
    ("ℕ", "double-struck capital n", "math natural numbers"),
    ("ℤ", "double-struck capital z", "math integers"),
    ("ℚ", "double-struck capital q", "math rational numbers"),
    ("ℂ", "double-struck capital c", "math complex numbers"),
    ("½", "vulgar fraction one half", "fraction"),
    ("⅓", "vulgar fraction one third", "fraction"),
    ("¼", "vulgar fraction one quarter", "fraction"),
    ("¾", "vulgar fraction three quarters", "fraction"),
    // Currency
    ("€", "euro sign", "currency money"),
    ("£", "pound sign", "currency money sterling"),
    ("¥", "yen sign", "currency money yuan"),
    ("¢", "cent sign", "currency money"),
    ("₹", "indian rupee sign", "currency money"),
    ("₽", "ruble sign", "currency money russian"),
    ("₩", "won sign", "currency money korean"),
    ("₿", "bitcoin sign", "currency money crypto"),
    ("₺", "turkish lira sign", "currency money"),
    ("₪", "new sheqel sign", "currency money israeli"),
    ("₫", "dong sign", "currency money vietnamese"),
    ("₴", "hryvnia sign", "currency money ukrainian"),
    ("₱", "peso sign", "currency money"),
    ("฿", "thai baht sign", "currency money"),
    ("¤", "currency sign", "money generic"),
    ("$", "dollar sign", "currency money"),
    // Punctuation and typography
    ("…", "horizontal ellipsis", "punctuation dots"),
    ("—", "em dash", "punctuation dash"),
    ("–", "en dash", "punctuation dash range"),
    ("‐", "hyphen", "punctuation"),
    ("•", "bullet", "punctuation list dot"),
    ("·", "middle dot", "punctuation interpunct"),
    ("‣", "triangular bullet", "punctuation list"),
    ("“", "left double quotation mark", "punctuation quote"),
    ("”", "right double quotation mark", "punctuation quote"),
    ("‘", "left single quotation mark", "punctuation quote"),
    (
        "’",
        "right single quotation mark",
        "punctuation quote apostrophe",
    ),
    (
        "«",
        "left-pointing double angle quotation mark",
        "punctuation quote guillemet",
    ),
    (
        "»",
        "right-pointing double angle quotation mark",
        "punctuation quote guillemet",
    ),
    ("„", "double low-9 quotation mark", "punctuation quote"),
    (
        "‹",
        "single left-pointing angle quotation mark",
        "punctuation quote",
    ),
    (
        "›",
        "single right-pointing angle quotation mark",
        "punctuation quote",
    ),
    ("¡", "inverted exclamation mark", "punctuation spanish"),
    ("¿", "inverted question mark", "punctuation spanish"),
    ("‽", "interrobang", "punctuation question exclamation"),
    ("§", "section sign", "punctuation legal"),
    ("¶", "pilcrow sign", "punctuation paragraph"),
    ("†", "dagger", "punctuation footnote"),
    ("‡", "double dagger", "punctuation footnote"),
    ("©", "copyright sign", "legal"),
    ("®", "registered sign", "legal trademark"),
    ("™", "trade mark sign", "legal trademark"),
    ("℠", "service mark", "legal"),
    ("№", "numero sign", "number"),
    ("℃", "degree celsius", "temperature"),
    ("℉", "degree fahrenheit", "temperature"),
    ("µ", "micro sign", "unit"),
    ("Ω", "ohm sign", "unit resistance"),
    ("Å", "angstrom sign", "unit"),
    ("\u{00A0}", "no-break space", "whitespace nbsp"),
    ("\u{2009}", "thin space", "whitespace"),
    ("\u{200B}", "zero width space", "whitespace invisible"),
    // Greek lowercase
    ("α", "greek small letter alpha", "greek"),
    ("β", "greek small letter beta", "greek"),
    ("γ", "greek small letter gamma", "greek"),
    ("δ", "greek small letter delta", "greek"),
    ("ε", "greek small letter epsilon", "greek"),
    ("ζ", "greek small letter zeta", "greek"),
    ("η", "greek small letter eta", "greek"),
    ("θ", "greek small letter theta", "greek"),
    ("ι", "greek small letter iota", "greek"),
    ("κ", "greek small letter kappa", "greek"),
    ("λ", "greek small letter lamda", "greek lambda"),
    ("μ", "greek small letter mu", "greek micro"),
    ("ν", "greek small letter nu", "greek"),
    ("ξ", "greek small letter xi", "greek"),
    ("ο", "greek small letter omicron", "greek"),
    ("ρ", "greek small letter rho", "greek"),
    ("σ", "greek small letter sigma", "greek"),
    ("τ", "greek small letter tau", "greek"),
    ("υ", "greek small letter upsilon", "greek"),
    ("φ", "greek small letter phi", "greek"),
    ("χ", "greek small letter chi", "greek"),
    ("ψ", "greek small letter psi", "greek"),
    ("ω", "greek small letter omega", "greek"),
    // Greek uppercase
    ("Γ", "greek capital letter gamma", "greek"),
    ("Δ", "greek capital letter delta", "greek"),
    ("Θ", "greek capital letter theta", "greek"),
    ("Λ", "greek capital letter lamda", "greek lambda"),
    ("Ξ", "greek capital letter xi", "greek"),
    ("Π", "greek capital letter pi", "greek"),
    ("Σ", "greek capital letter sigma", "greek"),
    ("Φ", "greek capital letter phi", "greek"),
    ("Ψ", "greek capital letter psi", "greek"),
    // Superscripts and subscripts
    ("⁰", "superscript zero", "superscript power"),
    ("¹", "superscript one", "superscript power"),
    ("²", "superscript two", "superscript power squared"),
    ("³", "superscript three", "superscript power cubed"),
    ("⁴", "superscript four", "superscript power"),
    ("⁵", "superscript five", "superscript power"),
    ("⁶", "superscript six", "superscript power"),
    ("⁷", "superscript seven", "superscript power"),
    ("⁸", "superscript eight", "superscript power"),
    ("⁹", "superscript nine", "superscript power"),
    ("⁺", "superscript plus sign", "superscript"),
    ("⁻", "superscript minus", "superscript"),
    ("ⁿ", "superscript latin small letter n", "superscript"),
    ("₀", "subscript zero", "subscript"),
    ("₁", "subscript one", "subscript"),
    ("₂", "subscript two", "subscript"),
    ("₃", "subscript three", "subscript"),
    ("₄", "subscript four", "subscript"),
    ("₅", "subscript five", "subscript"),
    ("₆", "subscript six", "subscript"),
    ("₇", "subscript seven", "subscript"),
    ("₈", "subscript eight", "subscript"),
    ("₉", "subscript nine", "subscript"),
    // Box drawing
    ("─", "box drawings light horizontal", "box line"),
    ("│", "box drawings light vertical", "box line"),
    ("┌", "box drawings light down and right", "box corner"),
    ("┐", "box drawings light down and left", "box corner"),
    ("└", "box drawings light up and right", "box corner"),
    ("┘", "box drawings light up and left", "box corner"),
    ("├", "box drawings light vertical and right", "box tee"),
    ("┤", "box drawings light vertical and left", "box tee"),
    ("┬", "box drawings light down and horizontal", "box tee"),
    ("┴", "box drawings light up and horizontal", "box tee"),
    (
        "┼",
        "box drawings light vertical and horizontal",
        "box cross",
    ),
    ("═", "box drawings double horizontal", "box line double"),
    ("║", "box drawings double vertical", "box line double"),
    ("█", "full block", "box block"),
    ("░", "light shade", "box block"),
    ("▒", "medium shade", "box block"),
    ("▓", "dark shade", "box block"),
    // Shapes and marks
    ("●", "black circle", "shape dot"),
    ("○", "white circle", "shape"),
    ("■", "black square", "shape"),
    ("□", "white square", "shape"),
    ("▲", "black up-pointing triangle", "shape"),
    ("▼", "black down-pointing triangle", "shape"),
    ("◆", "black diamond", "shape"),
    ("★", "black star", "shape"),
    ("☆", "white star", "shape"),
    ("✓", "check mark", "tick yes done"),
    ("✔", "heavy check mark", "tick yes done"),
    ("✗", "ballot x", "cross no"),
    ("✘", "heavy ballot x", "cross no"),
    ("♠", "black spade suit", "card"),
    ("♥", "black heart suit", "card love"),
    ("♦", "black diamond suit", "card"),
    ("♣", "black club suit", "card"),
    ("♪", "eighth note", "music"),
    ("♫", "beamed eighth notes", "music"),
    ("♯", "music sharp sign", "music"),
    ("♭", "music flat sign", "music"),
    // Keyboard keys
    ("⌘", "place of interest sign", "keyboard command cmd mac"),
    ("⌥", "option key", "keyboard option alt mac"),
    ("⇧", "upwards white arrow", "keyboard shift"),
    ("⌃", "up arrowhead", "keyboard control ctrl"),
    ("⏎", "return symbol", "keyboard enter return"),
    ("⌫", "erase to the left", "keyboard backspace delete"),
    ("⌦", "erase to the right", "keyboard delete"),
    ("⇥", "rightwards arrow to bar", "keyboard tab"),
    ("⇪", "upwards white arrow from bar", "keyboard caps lock"),
    (
        "⎋",
        "broken circle with northwest arrow",
        "keyboard escape esc",
    ),
    ("␣", "open box", "keyboard space"),
    (
        "⌤",
        "up arrowhead between two horizontal bars",
        "keyboard enter",
    ),
];

/// The curated symbol table, built once.
pub fn symbol_entries() -> &'static [GridEntry] {
    static TABLE: OnceLock<Vec<GridEntry>> = OnceLock::new();
    TABLE.get_or_init(|| {
        SYMBOLS
            .iter()
            .map(|(glyph, name, keywords)| GridEntry {
                glyph: glyph.to_string(),
                name: name.to_string(),
                keywords: keywords.split_whitespace().map(String::from).collect(),
            })
            .collect()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn found(entries: &[GridEntry], query: &str, glyph: &str) -> bool {
        search_entries(entries, query)
            .into_iter()
            .any(|i| entries[i].glyph == glyph)
    }

    #[test]
    fn emoji_search_finds_by_name() {
        let emoji = emoji_entries();
        assert!(found(&emoji, "grin", "😀"));
        assert!(found(&emoji, "GRIN", "😀"));
    }

    #[test]
    fn symbol_search_finds_by_keywords() {
        let symbols = symbol_entries();
        assert!(found(symbols, "arrow right", "→"));
        assert!(found(symbols, "euro", "€"));
        assert!(!found(symbols, "arrow right", "←"));
    }

    #[test]
    fn empty_query_matches_everything() {
        let symbols = symbol_entries();
        assert_eq!(search_entries(symbols, "  ").len(), symbols.len());
    }

    #[test]
    fn emoji_set_is_large_and_without_skin_tones() {
        let emoji = emoji_entries();
        assert!(emoji.len() > 1500, "only {} emoji", emoji.len());
        assert!(emoji
            .iter()
            .all(|e| !e.glyph.chars().any(is_skin_tone_modifier)));
    }

    #[test]
    fn symbol_table_has_no_duplicates_and_is_substantial() {
        let symbols = symbol_entries();
        assert!(symbols.len() >= 200, "only {}", symbols.len());
        let mut seen = std::collections::HashSet::new();
        for s in symbols {
            assert!(seen.insert(&s.glyph), "duplicate {:?}", s.glyph);
        }
    }
}
