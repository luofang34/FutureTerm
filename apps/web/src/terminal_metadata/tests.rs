#![allow(clippy::panic, clippy::unwrap_used, clippy::indexing_slicing)]

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
    // Note: "Line2" has no trailing newline, so "Line3" continues on same line
    meta.record_write(bytes1, "Line1\nLine2", 1000); // span1: Lines 0-1 (covers 0-2 exclusive)
    meta.record_write(bytes2, "Line3", 2000); // span2: Line 1 continuation (covers 1-2 exclusive)

    // Query line 0 only - should return only span1
    let result = meta.terminal_lines_to_bytes(0, 1);
    assert_eq!(result, Some((0, 11)));

    // Query lines 0-2 - both spans overlap this range
    let result = meta.terminal_lines_to_bytes(0, 2);
    assert_eq!(result, Some((0, 16)));

    // Query line 1 only - both spans cover line 1
    let result = meta.terminal_lines_to_bytes(1, 2);
    assert_eq!(result, Some((0, 16)));
}

#[test]
fn test_bytes_to_terminal_lines() {
    let mut meta = TerminalMetadata::new();

    let bytes1 = b"Line1\nLine2";
    let bytes2 = b"Line3";
    // Note: "Line2" has no trailing newline, so "Line3" continues on same line
    meta.record_write(bytes1, "Line1\nLine2", 1000); // span1: Lines 0-1, bytes 0-11
    meta.record_write(bytes2, "Line3", 2000); // span2: Line 1 continuation, bytes 11-16

    // Query bytes 0-11 (span1 only)
    let result = meta.bytes_to_terminal_lines(0, 11);
    assert_eq!(result, Some((0, 2))); // span1 covers lines 0-2 (exclusive)

    // Query bytes 11-16 (span2 only)
    let result = meta.bytes_to_terminal_lines(11, 16);
    assert_eq!(result, Some((1, 2))); // span2 covers lines 1-2 (exclusive)

    // Query all bytes (both spans)
    let result = meta.bytes_to_terminal_lines(0, 16);
    assert_eq!(result, Some((0, 2))); // Combined: lines 0-2 (exclusive)
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

// ==========================================
// build_char_map tests
// ==========================================

#[test]
fn test_build_char_map_plain_ascii() {
    // "Hello" -> 5 visible chars at columns 0-4
    let raw = b"Hello";
    let map = TerminalMetadata::build_char_map(raw, "Hello", 0);
    assert_eq!(map.len(), 5);
    for (i, entry) in map.iter().enumerate() {
        assert_eq!(entry.terminal_column, i);
        assert_eq!(entry.line_in_span, 0);
        assert_eq!(entry.byte_offset_in_span, i);
        assert_eq!(entry.byte_length, 1);
    }
}

#[test]
fn test_build_char_map_with_ansi_colors() {
    // "\x1b[31mRed\x1b[0m" -> 3 visible chars (R, e, d)
    // ANSI codes should be skipped entirely
    let raw = b"\x1b[31mRed\x1b[0m";
    let map = TerminalMetadata::build_char_map(raw, "Red", 0);
    assert_eq!(map.len(), 3);
    // R is at column 0
    assert_eq!(map[0].terminal_column, 0);
    assert_eq!(map[0].byte_offset_in_span, 5); // after "\x1b[31m" (5 bytes)
                                               // e is at column 1
    assert_eq!(map[1].terminal_column, 1);
    assert_eq!(map[1].byte_offset_in_span, 6);
    // d is at column 2
    assert_eq!(map[2].terminal_column, 2);
    assert_eq!(map[2].byte_offset_in_span, 7);
}

#[test]
fn test_build_char_map_with_newlines() {
    // "AB\nCD" -> 5 entries: A, B, \n, C, D
    // Line 0: A(col 0), B(col 1), \n(col 2)
    // Line 1: C(col 0), D(col 1)
    let raw = b"AB\nCD";
    let map = TerminalMetadata::build_char_map(raw, "AB\nCD", 0);
    assert_eq!(map.len(), 5);
    // A
    assert_eq!(map[0].line_in_span, 0);
    assert_eq!(map[0].terminal_column, 0);
    // B
    assert_eq!(map[1].line_in_span, 0);
    assert_eq!(map[1].terminal_column, 1);
    // \n
    assert_eq!(map[2].line_in_span, 0);
    assert_eq!(map[2].terminal_column, 2);
    assert_eq!(map[2].byte_offset_in_span, 2);
    // C (new line, column resets to 0)
    assert_eq!(map[3].line_in_span, 1);
    assert_eq!(map[3].terminal_column, 0);
    // D
    assert_eq!(map[4].line_in_span, 1);
    assert_eq!(map[4].terminal_column, 1);
}

#[test]
fn test_build_char_map_cr_lf() {
    // "\r\n" -> carriage return skipped, newline mapped
    let raw = b"\r\n";
    let map = TerminalMetadata::build_char_map(raw, "\r\n", 0);
    // Only \n should appear (CR is skipped)
    assert_eq!(map.len(), 1);
    assert_eq!(map[0].byte_offset_in_span, 1); // \n is at byte 1
    assert_eq!(map[0].line_in_span, 0);
}

#[test]
fn test_build_char_map_utf8_multibyte() {
    // "Ae\u{0301}" would be combining, let's use "A\u{00e9}" = "Aé"
    // 'A' is 1 byte, 'é' (U+00E9) is 2 bytes in UTF-8 (0xC3 0xA9)
    let raw = "Aé".as_bytes();
    let map = TerminalMetadata::build_char_map(raw, "Aé", 0);
    assert_eq!(map.len(), 2);
    // A: 1-byte ASCII
    assert_eq!(map[0].terminal_column, 0);
    assert_eq!(map[0].byte_offset_in_span, 0);
    assert_eq!(map[0].byte_length, 1);
    // é: 2-byte UTF-8
    assert_eq!(map[1].terminal_column, 1);
    assert_eq!(map[1].byte_offset_in_span, 1);
    assert_eq!(map[1].byte_length, 2);
}

#[test]
fn test_build_char_map_utf8_3byte() {
    // CJK character '中' (U+4E2D) is 3 bytes in UTF-8: 0xE4 0xB8 0xAD
    let raw = "A中B".as_bytes();
    let map = TerminalMetadata::build_char_map(raw, "A中B", 0);
    assert_eq!(map.len(), 3);
    // A: 1 byte ASCII
    assert_eq!(map[0].terminal_column, 0);
    assert_eq!(map[0].byte_offset_in_span, 0);
    assert_eq!(map[0].byte_length, 1);
    // 中: 3 bytes
    assert_eq!(map[1].terminal_column, 1);
    assert_eq!(map[1].byte_offset_in_span, 1);
    assert_eq!(map[1].byte_length, 3);
    // B: 1 byte ASCII (at byte offset 4)
    assert_eq!(map[2].terminal_column, 2);
    assert_eq!(map[2].byte_offset_in_span, 4);
    assert_eq!(map[2].byte_length, 1);
}

#[test]
fn test_build_char_map_utf8_4byte() {
    // Emoji '😀' (U+1F600) is 4 bytes in UTF-8: 0xF0 0x9F 0x98 0x80
    let raw = "A😀B".as_bytes();
    let map = TerminalMetadata::build_char_map(raw, "A😀B", 0);
    assert_eq!(map.len(), 3);
    // A: 1 byte
    assert_eq!(map[0].byte_length, 1);
    // 😀: 4 bytes
    assert_eq!(map[1].terminal_column, 1);
    assert_eq!(map[1].byte_offset_in_span, 1);
    assert_eq!(map[1].byte_length, 4);
    // B: 1 byte (at byte offset 5)
    assert_eq!(map[2].terminal_column, 2);
    assert_eq!(map[2].byte_offset_in_span, 5);
    assert_eq!(map[2].byte_length, 1);
}

#[test]
fn test_build_char_map_empty() {
    // "" -> empty mapping
    let raw = b"";
    let map = TerminalMetadata::build_char_map(raw, "", 0);
    assert!(map.is_empty());
}

#[test]
fn test_build_char_map_column_offset() {
    // Starting at column 5 -> first char at col 5
    let raw = b"XY";
    let map = TerminalMetadata::build_char_map(raw, "XY", 5);
    assert_eq!(map.len(), 2);
    assert_eq!(map[0].terminal_column, 5);
    assert_eq!(map[1].terminal_column, 6);
}

// ==========================================
// Terminal position mapping tests
// ==========================================

#[test]
fn test_position_to_bytes_single_line() {
    // Single-line content, select columns 2-4
    let mut meta = TerminalMetadata::new();
    meta.record_write(b"ABCDE", "ABCDE", 0);

    // Select columns 2-5 (end_col is exclusive per xterm convention)
    let result = meta.terminal_position_to_bytes(0, 2, 0, 5);
    assert!(result.is_some());
    let (start, end) = result.unwrap();
    assert_eq!(start, 2); // byte for 'C'
    assert_eq!(end, 5); // byte after 'E'
}

#[test]
fn test_position_to_bytes_multiline() {
    // Multi-line content, select across lines
    let mut meta = TerminalMetadata::new();
    meta.record_write(b"AB\nCD\nEF", "AB\nCD\nEF", 0);

    // Select from row 0, col 1 (B) to row 2, col 2 (after F, exclusive)
    let result = meta.terminal_position_to_bytes(0, 1, 2, 2);
    assert!(result.is_some());
    let (start, end) = result.unwrap();
    assert_eq!(start, 1); // byte for 'B'
    assert_eq!(end, 8); // byte after 'F'
}

#[test]
fn test_position_to_bytes_with_ansi() {
    // Content with ANSI codes, verify byte positions include ANSI bytes
    let raw = b"\x1b[31mRed\x1b[0m";
    let mut meta = TerminalMetadata::new();
    meta.record_write(raw, "Red", 0);

    // Select 'R' only (col 0 to col 1 exclusive)
    let result = meta.terminal_position_to_bytes(0, 0, 0, 1);
    assert!(result.is_some());
    let (start, end) = result.unwrap();
    // 'R' is at byte offset 5 within the span, span starts at global offset 0
    assert_eq!(start, 5);
    assert_eq!(end, 6);
}

#[test]
fn test_bytes_to_position_roundtrip() {
    // terminal_position_to_bytes then bytes_to_terminal_position should roundtrip
    let mut meta = TerminalMetadata::new();
    meta.record_write(b"ABCDE", "ABCDE", 0);

    // Select columns 1-3 (B, C) -> end_col 3 is exclusive
    let bytes = meta.terminal_position_to_bytes(0, 1, 0, 3);
    assert!(bytes.is_some());
    let (sb, eb) = bytes.unwrap();

    let pos = meta.bytes_to_terminal_position(sb, eb);
    assert!(pos.is_some());
    let (sr, sc, er, ec) = pos.unwrap();
    assert_eq!(sr, 0);
    assert_eq!(sc, 1);
    assert_eq!(er, 0);
    assert_eq!(ec, 3); // exclusive end column
}

#[test]
fn test_has_content_empty() {
    let meta = TerminalMetadata::new();
    assert!(!meta.has_content());
}

#[test]
fn test_has_content_after_write() {
    let mut meta = TerminalMetadata::new();
    meta.record_write(b"x", "x", 0);
    assert!(meta.has_content());
}

#[test]
fn test_clear_resets_everything() {
    let mut meta = TerminalMetadata::new();
    meta.record_write(b"test", "test", 0);
    meta.clear();
    assert!(!meta.has_content());
    assert_eq!(meta.span_count(), 0);
    assert_eq!(meta.current_byte_offset(), 0);
}

// ==========================================
// Edge case tests
// ==========================================

#[test]
fn test_adjust_for_log_trim_removes_old_spans() {
    // Trim enough bytes to invalidate first span entirely
    let mut meta = TerminalMetadata::new();
    meta.record_write(b"AAAA", "AAAA", 1000); // bytes 0-4
    meta.record_write(b"BBBB", "BBBB", 2000); // bytes 4-8

    // Trim 5 bytes: first span (0-4) becomes (0-0) -> removed
    // Second span (4-8) becomes (0-3) -> survives
    meta.adjust_for_log_trim(5);
    assert_eq!(meta.span_count(), 1);
    assert_eq!(meta.current_byte_offset(), 3);
}

#[test]
fn test_adjust_for_log_trim_partial() {
    // Trim fewer bytes than first span -- span shrinks but survives
    let mut meta = TerminalMetadata::new();
    meta.record_write(b"ABCDEFGH", "ABCDEFGH", 1000); // bytes 0-8
    meta.record_write(b"IJKL", "IJKL", 2000); // bytes 8-12

    // Trim 3 bytes: first span (0-8) becomes (0-5) -> survives (still has content)
    meta.adjust_for_log_trim(3);
    assert_eq!(meta.span_count(), 2);
    assert_eq!(meta.current_byte_offset(), 9); // 12 - 3
}

#[test]
fn test_multiple_spans_same_line() {
    // Two writes on same line without newline between them
    // Column offset should carry forward
    let mut meta = TerminalMetadata::new();
    meta.record_write(b"Hello", "Hello", 1000);
    meta.record_write(b" World", " World", 2000);

    assert_eq!(meta.span_count(), 2);
    // Both spans should be on line 0
    // First span: col_offset=0, text="Hello" (5 visible chars)
    // Second span: col_offset=5, text=" World" (6 visible chars)
    // Current column should be 11 after both writes

    // Select " World" portion (cols 5-11, end exclusive)
    let result = meta.terminal_position_to_bytes(0, 5, 0, 11);
    assert!(result.is_some());
    let (start, end) = result.unwrap();
    // " World" starts at global byte 5, ends at 11
    assert_eq!(start, 5);
    assert_eq!(end, 11);
}

#[test]
fn test_char_map_valid_after_partial_trim() {
    let mut meta = TerminalMetadata::new();
    meta.record_write(b"AAAA", "AAAA", 1000); // bytes 0-4
    meta.record_write(b"BBBB", "BBBB", 2000); // bytes 4-8

    // Trim 2 bytes — first span shrinks from (0,4) to (0,2) but survives
    meta.adjust_for_log_trim(2);

    // The second span should still produce valid position mappings
    // Span 2 was at (4,8), now at (2,6)
    // Selecting "BBBB" should work via position mapping
    let result = meta.terminal_position_to_bytes(0, 4, 0, 8);
    assert!(result.is_some());
    let (start, end) = result.unwrap();
    // Second span's raw_log_byte_start shifted from 4 to 2
    assert_eq!(start, 2);
    assert_eq!(end, 6);
}

#[test]
fn test_position_mapping_after_trim() {
    let mut meta = TerminalMetadata::new();
    // Write "Hello\nWorld" (11 bytes)
    meta.record_write(b"Hello\nWorld", "Hello\nWorld", 1000);
    // Write "Test" (4 bytes, continues on line 1 at col 5)
    meta.record_write(b"Test", "Test", 2000);

    // Before trim: span1 bytes 0-11, span2 bytes 11-15
    // Trim 5 bytes
    meta.adjust_for_log_trim(5);
    // After trim: span1 bytes 0-6, span2 bytes 6-10

    // Verify we can still map positions in the second span
    let result = meta.terminal_position_to_bytes(1, 5, 1, 9);
    assert!(result.is_some());
    let (start, end) = result.unwrap();
    assert_eq!(start, 6); // span2 shifted from 11 to 6
    assert_eq!(end, 10); // span2 shifted from 15 to 10
}

#[test]
fn test_count_visible_chars_with_osc_sequence() {
    // OSC sequences (ESC ] ... BEL) should be skipped
    let s = "\x1b]0;Title\x07Hello";
    // Only "Hello" is visible = 5 chars
    assert_eq!(count_visible_chars(s), 5);
}

#[test]
fn test_count_visible_chars_plain() {
    assert_eq!(count_visible_chars("Hello"), 5);
    assert_eq!(count_visible_chars(""), 0);
}

#[test]
fn test_count_visible_chars_with_csi() {
    // CSI color code: ESC [ 31 m
    let s = "\x1b[31mRed\x1b[0m";
    assert_eq!(count_visible_chars(s), 3);
}

#[test]
fn test_count_visible_chars_newlines_skipped() {
    // Newlines and CR are not visible column positions
    assert_eq!(count_visible_chars("A\nB\r\nC"), 3);
}

#[test]
fn test_default_impl() {
    // TerminalMetadata::default() should behave like ::new()
    let meta = TerminalMetadata::default();
    assert!(!meta.has_content());
    assert_eq!(meta.span_count(), 0);
    assert_eq!(meta.current_byte_offset(), 0);
}

#[test]
fn test_osc_with_st_terminator() {
    // OSC sequence terminated by ESC \ instead of BEL
    let raw = b"\x1b]0;Title\x1b\\Hello";
    let map = TerminalMetadata::build_char_map(raw, "Hello", 0);
    assert_eq!(map.len(), 5);
    assert_eq!(map[0].terminal_column, 0);
}

#[test]
fn test_terminal_lines_empty_metadata() {
    let meta = TerminalMetadata::new();
    assert_eq!(meta.terminal_lines_to_bytes(0, 1), None);
    assert_eq!(meta.bytes_to_terminal_lines(0, 10), None);
    assert_eq!(meta.terminal_position_to_bytes(0, 0, 0, 1), None);
    assert_eq!(meta.bytes_to_terminal_position(0, 10), None);
}
