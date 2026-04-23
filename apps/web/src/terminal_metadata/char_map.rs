use super::CharByteMapping;

/// Count visible characters in a string (skipping ANSI escape sequences)
pub(super) fn count_visible_chars(s: &str) -> usize {
    let bytes = s.as_bytes();
    let mut count = 0;
    let mut idx = 0;

    while idx < bytes.len() {
        // Skip ANSI escape sequences
        if let Some(&byte) = bytes.get(idx) {
            if byte == 0x1B && idx + 1 < bytes.len() {
                if let Some(&next_byte) = bytes.get(idx + 1) {
                    if next_byte == b'[' {
                        // CSI sequence
                        idx += 2;
                        while idx < bytes.len() {
                            if let Some(&b) = bytes.get(idx) {
                                if b.is_ascii_alphabetic() {
                                    break;
                                }
                            }
                            idx += 1;
                        }
                        idx += 1; // Skip the terminating letter
                        continue;
                    } else if next_byte == b']' {
                        // OSC sequence
                        idx += 2;
                        while idx < bytes.len() {
                            if let Some(&b) = bytes.get(idx) {
                                if b == 0x07 {
                                    idx += 1;
                                    break;
                                } else if idx + 1 < bytes.len() {
                                    if let (Some(&b1), Some(&b2)) =
                                        (bytes.get(idx), bytes.get(idx + 1))
                                    {
                                        if b1 == 0x1B && b2 == b'\\' {
                                            idx += 2;
                                            break;
                                        }
                                    }
                                }
                            }
                            idx += 1;
                        }
                        continue;
                    }
                }
            }

            // Skip carriage return and newline (they're not visible column positions)
            if byte == b'\r' || byte == b'\n' {
                idx += 1;
                continue;
            }

            // Count visible character
            count += 1;
        }
        idx += 1;
    }

    count
}

impl super::TerminalMetadata {
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
    pub(super) fn build_char_map(
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
            if let Some(&byte) = raw_bytes.get(byte_idx) {
                if byte == 0x1B && byte_idx + 1 < raw_bytes.len() {
                    if let Some(&next_byte) = raw_bytes.get(byte_idx + 1) {
                        if next_byte == b'[' {
                            // ANSI CSI sequence: ESC [ ... (letter)
                            byte_idx += 2;
                            while byte_idx < raw_bytes.len() {
                                if let Some(&c) = raw_bytes.get(byte_idx) {
                                    byte_idx += 1;
                                    // CSI sequences end with a letter (0x40-0x7E range)
                                    if (0x40..=0x7E).contains(&c) {
                                        break;
                                    }
                                } else {
                                    break;
                                }
                            }
                            // Skip ANSI sequences (don't add to map, don't increment column)
                            continue;
                        } else if next_byte == b']' {
                            // OSC sequence: ESC ] ... ST (ESC \ or BEL)
                            byte_idx += 2;
                            while byte_idx < raw_bytes.len() {
                                if let Some(&b) = raw_bytes.get(byte_idx) {
                                    if b == 0x07 {
                                        // BEL terminator
                                        byte_idx += 1;
                                        break;
                                    } else if byte_idx + 1 < raw_bytes.len() {
                                        if let (Some(&b1), Some(&b2)) =
                                            (raw_bytes.get(byte_idx), raw_bytes.get(byte_idx + 1))
                                        {
                                            if b1 == 0x1B && b2 == b'\\' {
                                                // ESC \ terminator
                                                byte_idx += 2;
                                                break;
                                            }
                                        }
                                    }
                                }
                                byte_idx += 1;
                            }
                            continue;
                        }
                    }
                }
            }

            // Check for carriage return (skip, it's not a visible character)
            if raw_bytes.get(byte_idx) == Some(&b'\r') {
                byte_idx += 1;
                continue;
            }

            // Check for newline character
            if raw_bytes.get(byte_idx) == Some(&b'\n') {
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
            let char_len = if let Some(&byte) = raw_bytes.get(byte_idx) {
                if byte & 0x80 == 0 {
                    1 // ASCII (0xxxxxxx)
                } else if byte & 0xE0 == 0xC0 {
                    2 // 2-byte UTF-8 (110xxxxx)
                } else if byte & 0xF0 == 0xE0 {
                    3 // 3-byte UTF-8 (1110xxxx)
                } else if byte & 0xF8 == 0xF0 {
                    4 // 4-byte UTF-8 (11110xxx)
                } else {
                    1 // Invalid, treat as single byte
                }
            } else {
                1 // Out of bounds, treat as single byte
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
}
