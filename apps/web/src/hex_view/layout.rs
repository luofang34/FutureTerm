use leptos::*;

// Display constants
/// Height of each hex row in pixels
pub(super) const ROW_HEIGHT: f64 = 28.0;

/// Auto-scroll threshold distance from bottom in pixels
pub(super) const AUTO_SCROLL_THRESHOLD: f64 = 100.0;

/// Number of buffer rows to prevent white flashes during scroll
pub(super) const SCROLL_BUFFER_ROWS: usize = 5;

// Responsive layout breakpoints (with 30px hysteresis dead zones)
/// Minimum width in pixels for 32-byte row layout
pub(super) const WIDE_LAYOUT_MIN_WIDTH: f64 = 1150.0;
/// Width below which we switch from 32 to 16 bytes/row
pub(super) const WIDE_LAYOUT_HYSTERESIS: f64 = 1120.0;

/// Minimum width in pixels for 16-byte row layout
pub(super) const MEDIUM_LAYOUT_MIN_WIDTH: f64 = 650.0;
/// Width below which we switch from 16 to 8 bytes/row
pub(super) const MEDIUM_LAYOUT_HYSTERESIS: f64 = 620.0;

/// Minimum width in pixels for 8-byte row layout
pub(super) const NARROW_LAYOUT_MIN_WIDTH: f64 = 400.0;
/// Width below which we switch from 8 to 4 bytes/row
pub(super) const NARROW_LAYOUT_HYSTERESIS: f64 = 370.0;

/// Represents a single hex dump row (4, 8, 16, or 32 bytes)
#[derive(Clone, Debug, PartialEq)]
pub(super) struct HexRow {
    pub offset: usize,
    pub bytes: Vec<u8>,
}

impl HexRow {
    #[allow(dead_code)]
    pub fn ascii(&self) -> String {
        self.bytes
            .iter()
            .map(|&b| {
                if (32..=126).contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect()
    }

    /// Returns groups of up to 4 bytes each
    pub fn byte_groups(&self) -> Vec<Vec<u8>> {
        self.bytes.chunks(4).map(|chunk| chunk.to_vec()).collect()
    }
}

/// Origin of an in-flight HexView selection (hex column vs ASCII column).
#[derive(Clone, Copy, PartialEq, Debug)]
pub(super) enum SelectionOrigin {
    Hex,
    Ascii,
}

/// Inline CSS for hex-byte, ASCII-char, and selection-highlight styling.
/// Embedded in the top-level `HexView` `view!` macro.
pub(super) const HEX_STYLES: &str = r#"
.hex-byte {
    cursor: text;
    display: inline-block;
    padding: 0 1px;
    box-sizing: border-box;
}
.ascii-char {
    cursor: text;
    display: inline-block;
    padding: 0;
}
.bg-sync {
    background-color: rgba(80, 150, 250, 0.35);
}
.bg-term {
    background-color: rgba(86, 156, 214, 0.3);
}
.hex-byte::selection, .ascii-char::selection {
    background-color: rgba(80, 150, 250, 0.4);
}
.bg-sync::selection {
    background-color: transparent;
}
.hex-data-container::selection,
.hex-data-container > div::selection,
.ascii-container::selection {
    background-color: transparent;
}
"#;

pub fn icon() -> impl IntoView {
    view! {
        <svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M12 2l9 5v10l-9 5l-9-5V7z" />
            <text x="50%" y="54%" text-anchor="middle" dominant-baseline="middle" font-size="9" font-weight="bold" fill="currentColor" stroke="none">"0x"</text>
        </svg>
    }
}
