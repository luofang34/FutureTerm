use std::collections::VecDeque;

/// Count visible characters in a string (skipping ANSI escape sequences)
fn count_visible_chars(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut count = 0;
    let mut idx = 0;

    while idx < bytes.len() {
        // Skip ANSI escape sequences
        if bytes[idx] == 0x1B && idx + 1 < bytes.len() {
            if bytes[idx + 1] == b'[' {
                // CSI sequence
                idx += 2;
                while idx < bytes.len() && !bytes[idx].is_ascii_alphabetic() {
                    idx += 1;
                }
                idx += 1; // Skip the terminating letter
                continue;
            } else if bytes[idx + 1] == b']' {
                // OSC sequence
                idx += 2;
                while idx < bytes.len() {
                    if bytes[idx] == 0x07 {
                        idx += 1;
                        break;
                    } else if idx + 1 < bytes.len() && bytes[idx] == 0x1B && bytes[idx + 1] == b'\\'
                    {
                        idx += 2;
                        break;
                    }
                    idx += 1;
                }
                continue;
            }
        }

        // Skip carriage return and newline (they're not visible column positions)
        if bytes[idx] == b'\r' || bytes[idx] == b'\n' {
            idx += 1;
            continue;
        }

        // Count visible character
        count += 1;
        idx += 1;
    }

    count
}

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

    /// Parse raw bytes and build character-to-byte mapping.
    /// Accounts for ANSI escape sequences (invisible) and multi-byte UTF-8 characters.
    /// Tracks line numbers and resets column counter on newlines.
    ///
    /// # Arguments
    /// * `raw_bytes` - Raw bytes including ANSI codes
    /// * `decoded_text` - Decoded text (for validation and debugging)
    /// * `column_offset` - Starting column for first line (for continuing spans on same line)
    ///
    /// # Returns
    /// Vector of CharByteMapping entries for each visible character
    fn build_char_map(
        raw_bytes: &[u8],
        _decoded_text: &str,
        column_offset: usize,
    ) -> Vec<CharByteMapping> {
        let mut map = Vec::new();
        let mut byte_idx = 0;
        let mut col_idx = column_offset; // Start from the column offset
        let mut line_idx = 0;

        // Debug logging disabled in release builds for performance
        #[cfg(all(debug_assertions, target_arch = "wasm32"))]
        {
            web_sys::console::log_1(
                &format!(
                    "build_char_map: {} bytes, column_offset: {}",
                    raw_bytes.len(),
                    column_offset
                )
                .into(),
            );
        }

        while byte_idx < raw_bytes.len() {
            // Check for ANSI escape sequence: ESC [ ... (letter)
            if raw_bytes[byte_idx] == 0x1B && byte_idx + 1 < raw_bytes.len() {
                if raw_bytes[byte_idx + 1] == b'[' {
                    // ANSI CSI sequence: ESC [ ... (letter)
                    byte_idx += 2;
                    while byte_idx < raw_bytes.len() {
                        let c = raw_bytes[byte_idx];
                        byte_idx += 1;
                        // CSI sequences end with a letter (0x40-0x7E range)
                        if (0x40..=0x7E).contains(&c) {
                            break;
                        }
                    }
                    // Skip ANSI sequences (don't add to map, don't increment column)
                    continue;
                } else if raw_bytes[byte_idx + 1] == b']' {
                    // OSC sequence: ESC ] ... ST (ESC \ or BEL)
                    byte_idx += 2;
                    while byte_idx < raw_bytes.len() {
                        if raw_bytes[byte_idx] == 0x07 {
                            // BEL terminator
                            byte_idx += 1;
                            break;
                        } else if byte_idx + 1 < raw_bytes.len()
                            && raw_bytes[byte_idx] == 0x1B
                            && raw_bytes[byte_idx + 1] == b'\\'
                        {
                            // ESC \ terminator
                            byte_idx += 2;
                            break;
                        }
                        byte_idx += 1;
                    }
                    continue;
                }
            }

            // Check for carriage return (skip, it's not a visible character)
            if raw_bytes[byte_idx] == b'\r' {
                byte_idx += 1;
                continue;
            }

            // Check for newline character
            if raw_bytes[byte_idx] == b'\n' {
                // Add newline to map (it's a visible character in terms of layout)
                map.push(CharByteMapping {
                    line_in_span: line_idx,
                    terminal_column: col_idx,
                    byte_offset_in_span: byte_idx,
                    byte_length: 1,
                });

                byte_idx += 1;
                line_idx += 1; // Move to next line
                col_idx = 0; // Reset column counter (new lines start at 0, not column_offset)
                continue;
            }

            // Regular character (UTF-8)
            let char_start = byte_idx;
            let char_len = if raw_bytes[byte_idx] & 0x80 == 0 {
                1 // ASCII (0xxxxxxx)
            } else if raw_bytes[byte_idx] & 0xE0 == 0xC0 {
                2 // 2-byte UTF-8 (110xxxxx)
            } else if raw_bytes[byte_idx] & 0xF0 == 0xE0 {
                3 // 3-byte UTF-8 (1110xxxx)
            } else if raw_bytes[byte_idx] & 0xF8 == 0xF0 {
                4 // 4-byte UTF-8 (11110xxx)
            } else {
                1 // Invalid, treat as single byte
            };

            map.push(CharByteMapping {
                line_in_span: line_idx,
                terminal_column: col_idx,
                byte_offset_in_span: char_start,
                byte_length: char_len,
            });

            byte_idx += char_len;
            col_idx += 1; // Each character counts as 1 column
        }

        // Debug logging disabled in release builds for performance
        #[cfg(all(debug_assertions, target_arch = "wasm32"))]
        {
            web_sys::console::log_1(&format!("char_map: {} entries", map.len()).into());
        }

        map
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

        // Build character-to-byte mapping for column-level precision
        // Pass the current column offset so this span knows where it starts on the line
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

    /// Maps Terminal line range to raw_log byte range
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

    /// Maps terminal selection (row+column precision) to byte range.
    /// Uses character-to-byte mapping to handle ANSI codes and UTF-8.
    ///
    /// # Arguments
    /// * `start_row` - Start row in Terminal buffer
    /// * `start_col` - Start column in Terminal buffer
    /// * `end_row` - End row in Terminal buffer
    /// * `end_col` - End column in Terminal buffer (EXCLUSIVE, as per xterm.js convention)
    ///
    /// # Returns
    /// Option<(start_byte_offset, end_byte_offset)> if mapping is found
    pub fn terminal_position_to_bytes(
        &self,
        start_row: usize,
        start_col: usize,
        end_row: usize,
        end_col: usize,
    ) -> Option<(usize, usize)> {
        #[cfg(all(debug_assertions, target_arch = "wasm32"))]
        web_sys::console::log_1(
            &format!(
                "terminal_position_to_bytes: start=({}, {}) end=({}, {})",
                start_row, start_col, end_row, end_col
            )
            .into(),
        );

        if self.spans.is_empty() {
            #[cfg(target_arch = "wasm32")]
            web_sys::console::log_1(&"No spans available".into());
            return None;
        }

        // Find span that CONTAINS (start_row, start_col)
        // Multiple spans can be on the same line, so check column ranges too
        let start_span = self.spans.iter().find(|s| {
            let span_line_count = s.text.lines().count().max(1);
            let span_end_line = s.terminal_line + span_line_count;

            // Check if span contains the row
            if !(s.terminal_line <= start_row && start_row < span_end_line) {
                return false;
            }

            // Check if span contains the column (on line 0 of the span)
            let line_in_span = start_row.saturating_sub(s.terminal_line);
            s.char_to_byte_map
                .iter()
                .any(|m| m.line_in_span == line_in_span && m.terminal_column == start_col)
        })?;

        #[cfg(all(debug_assertions, target_arch = "wasm32"))]
        web_sys::console::log_1(
            &format!(
                "Found start_span: terminal_line={}, column_offset={}, byte_range={}-{}, \
                 text_len={}, char_map_entries={}",
                start_span.terminal_line,
                start_span.column_offset,
                start_span.raw_log_byte_start,
                start_span.raw_log_byte_end,
                start_span.text.len(),
                start_span.char_to_byte_map.len()
            )
            .into(),
        );

        // IMPORTANT: xterm's end column is EXCLUSIVE (like a range)
        // When xterm says cols 31-33, it means include 31 and 32, but NOT 33
        // So we adjust BEFORE finding the span
        let actual_end_col = end_col.saturating_sub(1);

        #[cfg(all(debug_assertions, target_arch = "wasm32"))]
        web_sys::console::log_1(
            &format!(
                "Adjusted end column: {} -> {} (xterm end is exclusive)",
                end_col, actual_end_col
            )
            .into(),
        );

        // Find span that CONTAINS (end_row, actual_end_col)
        let end_span = self.spans.iter().find(|s| {
            let span_line_count = s.text.lines().count().max(1);
            let span_end_line = s.terminal_line + span_line_count;

            // Check if span contains the row
            if !(s.terminal_line <= end_row && end_row < span_end_line) {
                return false;
            }

            // Check if span contains the ADJUSTED column
            let line_in_span = end_row.saturating_sub(s.terminal_line);
            s.char_to_byte_map
                .iter()
                .any(|m| m.line_in_span == line_in_span && m.terminal_column == actual_end_col)
        })?;

        #[cfg(all(debug_assertions, target_arch = "wasm32"))]
        web_sys::console::log_1(
            &format!(
                "Found end_span: terminal_line={}, column_offset={}, byte_range={}-{}, \
                 text_len={}, char_map_entries={}",
                end_span.terminal_line,
                end_span.column_offset,
                end_span.raw_log_byte_start,
                end_span.raw_log_byte_end,
                end_span.text.len(),
                end_span.char_to_byte_map.len()
            )
            .into(),
        );

        // Calculate line index within start span
        let start_line_in_span = start_row.saturating_sub(start_span.terminal_line);

        #[cfg(all(debug_assertions, target_arch = "wasm32"))]
        web_sys::console::log_1(
            &format!(
                "Looking for start char: line_in_span={}, col={}",
                start_line_in_span, start_col
            )
            .into(),
        );

        // Find character at (line_in_span, column) in start span
        let start_char_map = start_span
            .char_to_byte_map
            .iter()
            .find(|m| m.line_in_span == start_line_in_span && m.terminal_column == start_col)
            .or_else(|| {
                #[cfg(target_arch = "wasm32")]
                web_sys::console::log_1(&"Using fallback: last char on line".into());
                // Fallback: find last character on this line
                start_span
                    .char_to_byte_map
                    .iter()
                    .rev()
                    .find(|m| m.line_in_span == start_line_in_span)
            })?;

        let start_byte = start_span.raw_log_byte_start + start_char_map.byte_offset_in_span;

        #[cfg(all(debug_assertions, target_arch = "wasm32"))]
        web_sys::console::log_1(
            &format!(
                "Found start char: line={}, col={}, byte_offset={}, len={} -> global_byte={}",
                start_char_map.line_in_span,
                start_char_map.terminal_column,
                start_char_map.byte_offset_in_span,
                start_char_map.byte_length,
                start_byte
            )
            .into(),
        );

        // Calculate line index within end span
        let end_line_in_span = end_row.saturating_sub(end_span.terminal_line);

        #[cfg(all(debug_assertions, target_arch = "wasm32"))]
        web_sys::console::log_1(
            &format!(
                "Looking for end char: line_in_span={}, col={} (already adjusted from {} since \
                 xterm end is exclusive)",
                end_line_in_span, actual_end_col, end_col
            )
            .into(),
        );

        // Find character at (line_in_span, column) in end span
        let end_char_map = end_span
            .char_to_byte_map
            .iter()
            .find(|m| m.line_in_span == end_line_in_span && m.terminal_column == actual_end_col)
            .or_else(|| {
                #[cfg(target_arch = "wasm32")]
                web_sys::console::log_1(&"Using fallback: last char on line".into());
                // Fallback: find last character on this line
                end_span
                    .char_to_byte_map
                    .iter()
                    .rev()
                    .find(|m| m.line_in_span == end_line_in_span)
            })?;

        // Include the full character at end position
        let end_byte = end_span.raw_log_byte_start
            + end_char_map.byte_offset_in_span
            + end_char_map.byte_length;

        #[cfg(all(debug_assertions, target_arch = "wasm32"))]
        web_sys::console::log_1(
            &format!(
                "Found end char: line={}, col={}, byte_offset={}, len={} -> global_byte={} \
                 (inclusive)",
                end_char_map.line_in_span,
                end_char_map.terminal_column,
                end_char_map.byte_offset_in_span,
                end_char_map.byte_length,
                end_byte
            )
            .into(),
        );

        #[cfg(all(debug_assertions, target_arch = "wasm32"))]
        web_sys::console::log_1(&format!("Mapped to bytes: {}-{}", start_byte, end_byte).into());

        Some((start_byte, end_byte))
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

    /// Maps raw_log byte range to Terminal selection position (row/col)
    ///
    /// # Arguments
    /// * `start_byte` - Start byte offset in raw_log
    /// * `end_byte` - End byte offset in raw_log (exclusive)
    ///
    /// # Returns
    /// Option<(start_row, start_col, end_row, end_col)> if mapping is found
    pub fn bytes_to_terminal_position(
        &self,
        start_byte: usize,
        end_byte: usize,
    ) -> Option<(usize, usize, usize, usize)> {
        if self.spans.is_empty() {
            return None;
        }

        let mut start_pos = None;
        let mut end_pos = None;

        for span in &self.spans {
            // Check for overlap
            if span.raw_log_byte_end > start_byte && span.raw_log_byte_start < end_byte {
                // Find start position
                if start_pos.is_none() {
                    // If start_byte is before this span, clamp to span start
                    if start_byte <= span.raw_log_byte_start {
                        start_pos = Some((span.terminal_line, span.column_offset));
                    } else {
                        // Find specific char
                        let local_offset = start_byte - span.raw_log_byte_start;
                        // Find visible char containing this byte
                        if let Some(map) = span.char_to_byte_map.iter().find(|m| {
                            m.byte_offset_in_span <= local_offset
                                && (m.byte_offset_in_span + m.byte_length) > local_offset
                        }) {
                            start_pos =
                                Some((span.terminal_line + map.line_in_span, map.terminal_column));
                        }
                    }
                }

                // Update end position (keep processing spans until we cover range)
                // If end_byte is beyond this span, we take the span end
                if end_byte >= span.raw_log_byte_end {
                    // Use the last character in the char map for precise positioning
                    if let Some(last) = span.char_to_byte_map.last() {
                        end_pos = Some((
                            span.terminal_line + last.line_in_span,
                            last.terminal_column + 1, // Exclusive
                        ));
                    }
                } else {
                    // Find specific char inside span
                    let local_offset = end_byte - span.raw_log_byte_start;
                    // Find visible char ending at or after this byte
                    if let Some(map) = span.char_to_byte_map.iter().find(|m| {
                        m.byte_offset_in_span <= local_offset // Start of char is before end
                            && (m.byte_offset_in_span + m.byte_length) >= local_offset
                        // End of char is >= end
                    }) {
                        // Correct logic: If end_byte is 5, and char is 4-5, we want end of that
                        // char? Selection is exclusive.
                        end_pos = Some((
                            span.terminal_line + map.line_in_span,
                            map.terminal_column, /* xterm end is exclusive, so if map is col 5,
                                                  * and we end at start of col 5, it's 5.
                                                  * If we end strictly AFTER start of col 5...
                                                  * Simpler: Map byte to char index. */
                        ));

                        // Adjust for partial overlap?
                        // If local_offset == m.byte_offset_in_span, we end exactly at char start ->
                        // col If local_offset > m, we end inside -> col + 1
                        if local_offset > map.byte_offset_in_span {
                            end_pos = Some((
                                span.terminal_line + map.line_in_span,
                                map.terminal_column + 1,
                            ));
                        }
                    }
                }
            }
        }

        match (start_pos, end_pos) {
            (Some((sr, sc)), Some((er, ec))) => Some((sr, sc, er, ec)),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_basic_write_tracking() {
        let mut meta = TerminalMetadata::new();

        // Write first frame
        let bytes1 = b"Hello\nWorld";
        meta.record_write(bytes1, "Hello\nWorld", 1000);
        assert_eq!(meta.current_byte_offset(), 11);
        assert_eq!(meta.span_count(), 1);

        // Write second frame
        let bytes2 = b"Test";
        meta.record_write(bytes2, "Test", 2000);
        assert_eq!(meta.current_byte_offset(), 15);
        assert_eq!(meta.span_count(), 2);
    }

    #[test]
    fn test_terminal_lines_to_bytes() {
        let mut meta = TerminalMetadata::new();

        let bytes1 = b"Line1\nLine2";
        let bytes2 = b"Line3";
        meta.record_write(bytes1, "Line1\nLine2", 1000); // Lines 0-1
        meta.record_write(bytes2, "Line3", 2000); // Line 2

        // Query lines 0-1 (first span)
        let result = meta.terminal_lines_to_bytes(0, 2);
        assert_eq!(result, Some((0, 11)));

        // Query line 2 (second span)
        let result = meta.terminal_lines_to_bytes(2, 3);
        assert_eq!(result, Some((11, 16)));

        // Query all lines
        let result = meta.terminal_lines_to_bytes(0, 3);
        assert_eq!(result, Some((0, 16)));
    }

    #[test]
    fn test_bytes_to_terminal_lines() {
        let mut meta = TerminalMetadata::new();

        let bytes1 = b"Line1\nLine2";
        let bytes2 = b"Line3";
        meta.record_write(bytes1, "Line1\nLine2", 1000); // Lines 0-1, bytes 0-11
        meta.record_write(bytes2, "Line3", 2000); // Line 2, bytes 11-16

        // Query bytes 0-11 (first span)
        let result = meta.bytes_to_terminal_lines(0, 11);
        assert_eq!(result, Some((0, 2)));

        // Query bytes 11-16 (second span)
        let result = meta.bytes_to_terminal_lines(11, 16);
        assert_eq!(result, Some((2, 3)));

        // Query all bytes
        let result = meta.bytes_to_terminal_lines(0, 16);
        assert_eq!(result, Some((0, 3)));
    }

    #[test]
    fn test_log_trim_adjustment() {
        let mut meta = TerminalMetadata::new();

        let bytes1 = b"Test1";
        let bytes2 = b"Test2";
        meta.record_write(bytes1, "Test1", 1000);
        meta.record_write(bytes2, "Test2", 2000);

        // Trim first 5 bytes
        meta.adjust_for_log_trim(5);

        assert_eq!(meta.current_byte_offset(), 5);
        assert_eq!(meta.span_count(), 1); // First span trimmed away

        // Verify offsets adjusted correctly
        let result = meta.bytes_to_terminal_lines(0, 5);
        assert!(result.is_some());
    }

    #[test]
    fn test_max_capacity() {
        let mut meta = TerminalMetadata::with_capacity(3);

        meta.record_write(b"A", "A", 1000);
        meta.record_write(b"B", "B", 2000);
        meta.record_write(b"C", "C", 3000);
        meta.record_write(b"D", "D", 4000); // Should evict oldest

        assert_eq!(meta.span_count(), 3);
    }
}
