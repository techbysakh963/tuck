//! Paste modes shared by the popup's keyboard handling.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteMode {
    Normal,
    ForceTerminal,
}
