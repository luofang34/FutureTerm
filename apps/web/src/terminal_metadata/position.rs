use super::TerminalSpan;

impl super::TerminalMetadata {
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
            #[cfg(all(debug_assertions, target_arch = "wasm32"))]
            web_sys::console::log_1(&"No spans available".into());
            return None;
        }

        // Helper to find span for a position with fallback
        // If exact column match fails (e.g. selection past end of line), returns the last span on that row.
        let find_span_clamped = |row: usize, col: usize| -> Option<&TerminalSpan> {
            // 1. Try exact match
            if let Some(s) = self.spans.iter().find(|s| {
                let span_line_count = s.text.lines().count().max(1);
                let span_end_line = s.terminal_line + span_line_count;
                if !(s.terminal_line <= row && row < span_end_line) {
                    return false;
                }

                let line_in_span = row.saturating_sub(s.terminal_line);
                s.char_to_byte_map
                    .iter()
                    .any(|m| m.line_in_span == line_in_span && m.terminal_column == col)
            }) {
                return Some(s);
            }

            // 2. Fallback: Find last span on this row (handles overshoot)
            self.spans.iter().rfind(|s| {
                let span_line_count = s.text.lines().count().max(1);
                let span_end_line = s.terminal_line + span_line_count;
                s.terminal_line <= row && row < span_end_line
            })
        };

        // Find start span
        let start_span = find_span_clamped(start_row, start_col)?;

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

        // Find end span
        let end_span = find_span_clamped(end_row, actual_end_col)?;

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
                // Fallback: find last character on this line (clamped)
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
                // Fallback: find last character on this line (clamped)
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
}
