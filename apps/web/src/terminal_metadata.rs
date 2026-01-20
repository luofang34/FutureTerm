use std::collections::VecDeque;

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

    /// Line number in Terminal buffer (dynamic, invalidated on scroll/clear)
    pub terminal_line: usize,

    /// Timestamp in microseconds
    pub timestamp_us: u64,
}

/// Tracks metadata for Terminal text to enable byte-level selection mapping.
/// Maintains a sliding window of recent spans to correlate Terminal positions
/// with raw_log byte offsets.
pub struct TerminalMetadata {
    spans: VecDeque<TerminalSpan>,
    current_raw_log_offset: usize, // Cumulative byte offset
    current_terminal_line: usize,  // Current Terminal line number
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
            max_spans,
        }
    }

    /// Records a new write operation to the Terminal
    ///
    /// # Arguments
    /// * `frame_bytes_len` - Number of raw bytes in this frame
    /// * `text` - Decoded text written to Terminal
    /// * `timestamp_us` - Timestamp of this write
    pub fn record_write(&mut self, frame_bytes_len: usize, text: &str, timestamp_us: u64) {
        let line_count = text.lines().count().max(1); // At least 1 line

        let span = TerminalSpan {
            raw_log_byte_start: self.current_raw_log_offset,
            raw_log_byte_end: self.current_raw_log_offset + frame_bytes_len,
            text: text.to_string(),
            terminal_line: self.current_terminal_line,
            timestamp_us,
        };

        self.current_raw_log_offset += frame_bytes_len;
        self.current_terminal_line += line_count;

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

    /// Clears all metadata (e.g., when Terminal is reset)
    #[allow(dead_code)] // Reserved for future Terminal reset handling
    pub fn clear(&mut self) {
        self.spans.clear();
        self.current_raw_log_offset = 0;
        self.current_terminal_line = 0;
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
        meta.record_write(10, "Hello\nWorld", 1000);
        assert_eq!(meta.current_byte_offset(), 10);
        assert_eq!(meta.span_count(), 1);

        // Write second frame
        meta.record_write(5, "Test", 2000);
        assert_eq!(meta.current_byte_offset(), 15);
        assert_eq!(meta.span_count(), 2);
    }

    #[test]
    fn test_terminal_lines_to_bytes() {
        let mut meta = TerminalMetadata::new();

        meta.record_write(10, "Line1\nLine2", 1000); // Lines 0-1
        meta.record_write(5, "Line3", 2000); // Line 2

        // Query lines 0-1 (first span)
        let result = meta.terminal_lines_to_bytes(0, 2);
        assert_eq!(result, Some((0, 10)));

        // Query line 2 (second span)
        let result = meta.terminal_lines_to_bytes(2, 3);
        assert_eq!(result, Some((10, 15)));

        // Query all lines
        let result = meta.terminal_lines_to_bytes(0, 3);
        assert_eq!(result, Some((0, 15)));
    }

    #[test]
    fn test_bytes_to_terminal_lines() {
        let mut meta = TerminalMetadata::new();

        meta.record_write(10, "Line1\nLine2", 1000); // Lines 0-1, bytes 0-10
        meta.record_write(5, "Line3", 2000); // Line 2, bytes 10-15

        // Query bytes 0-10 (first span)
        let result = meta.bytes_to_terminal_lines(0, 10);
        assert_eq!(result, Some((0, 2)));

        // Query bytes 10-15 (second span)
        let result = meta.bytes_to_terminal_lines(10, 15);
        assert_eq!(result, Some((2, 3)));

        // Query all bytes
        let result = meta.bytes_to_terminal_lines(0, 15);
        assert_eq!(result, Some((0, 3)));
    }

    #[test]
    fn test_log_trim_adjustment() {
        let mut meta = TerminalMetadata::new();

        meta.record_write(10, "Test1", 1000);
        meta.record_write(10, "Test2", 2000);

        // Trim first 5 bytes
        meta.adjust_for_log_trim(5);

        assert_eq!(meta.current_byte_offset(), 15);
        assert_eq!(meta.span_count(), 2); // Both spans still valid

        // Verify offsets adjusted correctly
        let result = meta.bytes_to_terminal_lines(0, 5);
        assert!(result.is_some());
    }

    #[test]
    fn test_max_capacity() {
        let mut meta = TerminalMetadata::with_capacity(3);

        meta.record_write(1, "A", 1000);
        meta.record_write(1, "B", 2000);
        meta.record_write(1, "C", 3000);
        meta.record_write(1, "D", 4000); // Should evict oldest

        assert_eq!(meta.span_count(), 3);
    }
}
