use std::collections::VecDeque;

use self::char_map::count_visible_chars;

/// Maps terminal column positions to byte positions within a span.
/// Accounts for ANSI escape sequences and multi-byte UTF-8 characters.
#[derive(Clone, Debug)]
pub struct CharByteMapping {
    /// Line number within this span (0-indexed, resets per span)
    pub line_in_span: usize,

    /// Terminal column position (visible character index within the line)
    pub terminal_column: usize,

    /// Byte offset within this span's raw_bytes
    pub byte_offset_in_span: usize,

    /// Byte length (1 for ASCII, 2-4 for UTF-8)
    pub byte_length: usize,
}

/// Terminal text span mapped to raw_log byte positions.
/// This allows mapping between Terminal buffer positions and raw byte offsets.
#[derive(Clone, Debug)]
#[allow(dead_code)] // Reserved for future cross-view selection sync
pub struct TerminalSpan {
    /// Start byte offset in raw_log
    pub raw_log_byte_start: usize,

    /// End byte offset in raw_log (exclusive)
    pub raw_log_byte_end: usize,

    /// Decoded text content
    pub text: String,

    /// Original raw bytes for this span (includes ANSI codes)
    pub raw_bytes: Vec<u8>,

    /// Line number in Terminal buffer (dynamic, invalidated on scroll/clear)
    pub terminal_line: usize,

    /// Starting column offset for this span on its first line
    /// (used when multiple spans exist on the same terminal line)
    pub column_offset: usize,

    /// Timestamp in microseconds
    pub timestamp_us: u64,

    /// Character to byte mapping (for column-level precision)
    pub char_to_byte_map: Vec<CharByteMapping>,
}

/// Tracks metadata for Terminal text to enable byte-level selection mapping.
/// Maintains a sliding window of recent spans to correlate Terminal positions
/// with raw_log byte offsets.
#[derive(Clone)]
pub struct TerminalMetadata {
    spans: VecDeque<TerminalSpan>,
    current_raw_log_offset: usize,  // Cumulative byte offset
    current_terminal_line: usize,   // Current Terminal line number
    current_terminal_column: usize, // Current column position on current line
    max_spans: usize,               // Maximum number of spans to retain
}

impl TerminalMetadata {
    /// Creates a new metadata tracker with default capacity (1000 spans)
    pub fn new() -> Self {
        Self::with_capacity(1000)
    }

    /// Creates a new metadata tracker with specified capacity
    pub fn with_capacity(max_spans: usize) -> Self {
        Self {
            spans: VecDeque::with_capacity(max_spans),
            current_raw_log_offset: 0,
            current_terminal_line: 0,
            current_terminal_column: 0,
            max_spans,
        }
    }

    /// Records a new write operation to the Terminal
    ///
    /// # Arguments
    /// * `raw_bytes` - Raw bytes in this frame (includes ANSI codes)
    /// * `text` - Decoded text written to Terminal
    /// * `timestamp_us` - Timestamp of this write
    pub fn record_write(&mut self, raw_bytes: &[u8], text: &str, timestamp_us: u64) {
        let frame_bytes_len = raw_bytes.len();

        // Count lines (including partial lines)
        let newline_count = text.matches('\n').count();
        let has_trailing_text = !text.ends_with('\n');

        // Build character-to-byte mapping eagerly (not lazily).
        // Leptos signal.get_untracked() returns a CLONE — lazy Option<Vec> maps
        // would always be None on the clone, rebuilt every query, then discarded.
        // Eager building here means the built Vec survives cloning.
        let char_map = Self::build_char_map(raw_bytes, text, self.current_terminal_column);

        let span = TerminalSpan {
            raw_log_byte_start: self.current_raw_log_offset,
            raw_log_byte_end: self.current_raw_log_offset + frame_bytes_len,
            text: text.to_string(),
            raw_bytes: raw_bytes.to_vec(),
            terminal_line: self.current_terminal_line,
            column_offset: self.current_terminal_column,
            timestamp_us,
            char_to_byte_map: char_map,
        };

        self.current_raw_log_offset += frame_bytes_len;

        // Update line and column position
        if newline_count > 0 {
            self.current_terminal_line += newline_count;
            // After newlines, column resets to 0, then advances by any trailing text
            if has_trailing_text {
                // Count visible characters in the last line (after last \n)
                let last_line = text.rsplit('\n').next().unwrap_or("");
                self.current_terminal_column = count_visible_chars(last_line);
            } else {
                self.current_terminal_column = 0;
            }
        } else {
            // No newlines - we're continuing on the same line
            // Advance column by the number of visible characters
            self.current_terminal_column += count_visible_chars(text);
        }

        self.spans.push_back(span);

        // Maintain maximum capacity
        if self.spans.len() > self.max_spans {
            self.spans.pop_front();
        }
    }

    /// Maps terminal line range to raw_log byte range
    ///
    /// # Arguments
    /// * `start_line` - Start line in Terminal buffer
    /// * `end_line` - End line in Terminal buffer (exclusive)
    ///
    /// # Returns
    /// Option<(start_byte_offset, end_byte_offset)> if mapping is found
    #[allow(dead_code)] // Reserved for future Terminal → Hex selection sync
    pub fn terminal_lines_to_bytes(
        &self,
        start_line: usize,
        end_line: usize,
    ) -> Option<(usize, usize)> {
        if self.spans.is_empty() {
            return None;
        }

        let mut start_byte_offset = None;
        let mut end_byte_offset = None;

        for span in &self.spans {
            let span_line_count = span.text.lines().count().max(1);
            let span_end_line = span.terminal_line + span_line_count;

            // Check if this span overlaps with the requested line range
            if span_end_line > start_line && span.terminal_line < end_line {
                // First overlapping span sets start offset
                if start_byte_offset.is_none() {
                    start_byte_offset = Some(span.raw_log_byte_start);
                }
                // Keep updating end offset as we find overlapping spans
                end_byte_offset = Some(span.raw_log_byte_end);
            }
        }

        match (start_byte_offset, end_byte_offset) {
            (Some(start), Some(end)) => Some((start, end)),
            _ => None,
        }
    }

    /// Maps raw_log byte range to Terminal line range
    ///
    /// # Arguments
    /// * `start_byte` - Start byte offset in raw_log
    /// * `end_byte` - End byte offset in raw_log (exclusive)
    ///
    /// # Returns
    /// Option<(start_line, end_line)> if mapping is found
    #[allow(dead_code)] // Reserved for future Hex → Terminal selection sync
    pub fn bytes_to_terminal_lines(
        &self,
        start_byte: usize,
        end_byte: usize,
    ) -> Option<(usize, usize)> {
        if self.spans.is_empty() {
            return None;
        }

        let mut start_line = None;
        let mut end_line = None;

        for span in &self.spans {
            // Check if this span overlaps with the requested byte range
            if span.raw_log_byte_end > start_byte && span.raw_log_byte_start < end_byte {
                let span_line_count = span.text.lines().count().max(1);
                let span_end_line = span.terminal_line + span_line_count;

                // First overlapping span sets start line
                if start_line.is_none() {
                    start_line = Some(span.terminal_line);
                }
                // Keep updating end line as we find overlapping spans
                end_line = Some(span_end_line);
            }
        }

        match (start_line, end_line) {
            (Some(start), Some(end)) => Some((start, end)),
            _ => None,
        }
    }

    /// Adjusts all byte offsets when raw_log is trimmed
    ///
    /// # Arguments
    /// * `trimmed_bytes` - Number of bytes removed from the beginning of raw_log
    pub fn adjust_for_log_trim(&mut self, trimmed_bytes: usize) {
        for span in &mut self.spans {
            span.raw_log_byte_start = span.raw_log_byte_start.saturating_sub(trimmed_bytes);
            span.raw_log_byte_end = span.raw_log_byte_end.saturating_sub(trimmed_bytes);
        }
        self.current_raw_log_offset = self.current_raw_log_offset.saturating_sub(trimmed_bytes);

        // Remove spans that are completely invalidated (start == end after adjustment)
        self.spans
            .retain(|span| span.raw_log_byte_start < span.raw_log_byte_end);
    }

    /// Returns true if the terminal has any content (not at origin position)
    pub fn has_content(&self) -> bool {
        self.current_terminal_line > 0 || self.current_terminal_column > 0
    }

    /// Clears all metadata (e.g., when Terminal is reset)
    #[allow(dead_code)] // Reserved for future Terminal reset handling
    pub fn clear(&mut self) {
        self.spans.clear();
        self.current_raw_log_offset = 0;
        self.current_terminal_line = 0;
        self.current_terminal_column = 0;
    }

    /// Returns the current number of tracked spans
    #[allow(dead_code)] // Reserved for debugging/monitoring
    pub fn span_count(&self) -> usize {
        self.spans.len()
    }

    /// Returns the current cumulative byte offset
    #[allow(dead_code)] // Reserved for debugging/monitoring
    pub fn current_byte_offset(&self) -> usize {
        self.current_raw_log_offset
    }
}

impl Default for TerminalMetadata {
    fn default() -> Self {
        Self::new()
    }
}

mod char_map;
mod position;

#[cfg(test)]
mod tests;
