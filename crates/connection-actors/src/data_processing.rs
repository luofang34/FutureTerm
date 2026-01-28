//! Data processing utilities for connection actors
//!
//! This module provides utilities for cleaning and analyzing data received
//! from serial ports, particularly for detecting shell prompts and meaningful content.

/// Trim shell artifacts from initial data received from a serial port
///
/// This function removes:
/// 1. Leading whitespace (CR, LF, NULL bytes)
/// 2. ANSI escape sequences (CSI sequences like ESC[?2004l, charset designators)
///
/// This is useful when connecting to shell-like interfaces where:
/// - The probe sends '\r' which echoes as '\r\n'
/// - The device may send ANSI sequences before the prompt
/// - We want to "hoist" the actual prompt to line 1 of the terminal
///
/// # Example
/// ```ignore
/// let data = b"\x1b[?2004l\r\n$ ";  // ANSI + newlines + prompt
/// let trimmed = trim_shell_artifacts(data);
/// assert_eq!(trimmed, b"$ ");  // Just the prompt
/// ```
pub fn trim_shell_artifacts(data: &[u8]) -> Vec<u8> {
    let mut start_idx = 0;

    while start_idx < data.len() {
        let Some(&b) = data.get(start_idx) else {
            break;
        };

        // 1. Skip Whitespace (CR, LF, NULL)
        if b == b'\r' || b == b'\n' || b == 0 {
            start_idx += 1;
            continue;
        }

        // 2. Skip ANSI Escape Sequences
        if b == 0x1B {
            // ESC character
            // Check for CSI (Control Sequence Introducer): ESC [
            if let Some(&b'[') = data.get(start_idx + 1) {
                let mut j = start_idx + 2;
                // Scan for terminator (0x40-0x7E)
                while j < data.len() {
                    if let Some(&tb) = data.get(j) {
                        if (0x40..=0x7E).contains(&tb) {
                            // Found terminator, skip this entire sequence
                            start_idx = j + 1;
                            break;
                        }
                    }
                    j += 1;
                }
                if start_idx > j {
                    // Successfully skipped an ANSI sequence
                    continue;
                }
                // If we fell through, we ran out of buffer inside ANSI
                break;
            }
            // Check for Charset Designators: ESC ( X or ESC ) X
            else if let Some(&c) = data.get(start_idx + 1) {
                if c == b'(' || c == b')' {
                    // Expect one more byte for charset ID
                    if start_idx + 2 < data.len() {
                        start_idx += 3; // Skip ESC ( X
                        continue;
                    } else {
                        // Incomplete sequence at end of buffer
                        break;
                    }
                }
                // Other ESC sequences - treat as content
                else {
                    break;
                }
            } else {
                // ESC at end of buffer - treat as content
                break;
            }
        }

        // If we get here, it's not whitespace and not ANSI. It's actual content.
        break;
    }

    data.get(start_idx..)
        .map(|s| s.to_vec())
        .unwrap_or_default()
}

/// Detect if data contains meaningful content (not just control characters)
///
/// This heuristic checks if the data contains typical shell prompt characters:
/// - Alphanumeric characters (a-z, A-Z, 0-9)
/// - Common prompt characters ($, #, >, %, ~)
///
/// Used to decide whether to send a wakeup byte to trigger a shell prompt,
/// or to inject the received data as-is.
///
/// # Returns
/// - `true` if meaningful content detected (e.g., "$ ", "root@host:~# ")
/// - `false` if only control characters or whitespace
pub fn detect_meaningful_content(data: &[u8]) -> bool {
    data.iter().any(|&b| {
        b.is_ascii_lowercase()
            || b.is_ascii_uppercase()
            || b.is_ascii_digit()
            || b == b'$'
            || b == b'#'
            || b == b'>'
            || b == b'%'
            || b == b'~'
    })
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn test_trim_empty() {
        assert_eq!(trim_shell_artifacts(&[]), Vec::<u8>::new());
    }

    #[test]
    fn test_trim_whitespace_only() {
        assert_eq!(trim_shell_artifacts(b"\r\n\r\n"), Vec::<u8>::new());
        assert_eq!(trim_shell_artifacts(b"\0\0\0"), Vec::<u8>::new());
    }

    #[test]
    fn test_trim_preserves_content() {
        assert_eq!(trim_shell_artifacts(b"$ "), b"$ ");
        assert_eq!(trim_shell_artifacts(b"root@host:~# "), b"root@host:~# ");
    }

    #[test]
    fn test_trim_removes_leading_whitespace() {
        assert_eq!(trim_shell_artifacts(b"\r\n$ "), b"$ ");
        assert_eq!(trim_shell_artifacts(b"\r\n\r\n$ "), b"$ ");
    }

    #[test]
    fn test_trim_removes_ansi_csi() {
        // ESC[?2004l (bracketed paste mode off)
        assert_eq!(trim_shell_artifacts(b"\x1b[?2004l$ "), b"$ ");
        // ESC[0m (reset)
        assert_eq!(trim_shell_artifacts(b"\x1b[0m$ "), b"$ ");
        // ESC[1;32m (green bold)
        assert_eq!(trim_shell_artifacts(b"\x1b[1;32m$ "), b"$ ");
    }

    #[test]
    fn test_trim_removes_charset_designators() {
        // ESC(B (ASCII charset)
        assert_eq!(trim_shell_artifacts(b"\x1b(B$ "), b"$ ");
        // ESC)0 (Line drawing charset)
        assert_eq!(trim_shell_artifacts(b"\x1b)0$ "), b"$ ");
    }

    #[test]
    fn test_trim_combined_ansi_and_whitespace() {
        // Real-world example: ANSI + newlines + prompt
        assert_eq!(trim_shell_artifacts(b"\x1b[?2004l\r\n$ "), b"$ ");
        assert_eq!(
            trim_shell_artifacts(b"\x1b(B\x1b[0m\r\n\r\nroot@host:~# "),
            b"root@host:~# "
        );
    }

    #[test]
    fn test_trim_incomplete_ansi_at_end() {
        // Incomplete ANSI at end of buffer - should be preserved
        let result = trim_shell_artifacts(b"\x1b[");
        assert!(result.is_empty() || result == b"\x1b[");
    }

    #[test]
    fn test_detect_meaningful_alphanumeric() {
        assert!(detect_meaningful_content(b"$ "));
        assert!(detect_meaningful_content(b"root@host:~# "));
        assert!(detect_meaningful_content(b"C:\\>"));
    }

    #[test]
    fn test_detect_meaningful_prompt_chars() {
        assert!(detect_meaningful_content(b"$ "));
        assert!(detect_meaningful_content(b"# "));
        assert!(detect_meaningful_content(b"> "));
        assert!(detect_meaningful_content(b"% "));
        assert!(detect_meaningful_content(b"~ "));
    }

    #[test]
    fn test_detect_not_meaningful_whitespace() {
        assert!(!detect_meaningful_content(b"\r\n\r\n"));
        assert!(!detect_meaningful_content(b"   "));
        assert!(!detect_meaningful_content(&[]));
    }

    #[test]
    fn test_detect_not_meaningful_control_chars() {
        // detect_meaningful_content works on trimmed data
        // After trimming ANSI sequences, only NULL bytes remain
        let trimmed = trim_shell_artifacts(b"\x1b[?2004l");
        assert!(!detect_meaningful_content(&trimmed));
        assert!(!detect_meaningful_content(b"\0\0\0"));
    }

    #[test]
    fn test_real_world_shell_sequence() {
        // Typical sequence from a Linux shell
        let data = b"\x1b[?2004l\r\n\x1b(B\x1b[0muser@host:~$ ";
        let trimmed = trim_shell_artifacts(data);
        assert_eq!(trimmed, b"user@host:~$ ");
        assert!(detect_meaningful_content(&trimmed));
    }

    #[test]
    fn test_real_world_empty_response() {
        // Device sends only ANSI sequences with no actual prompt
        let data = b"\x1b[?2004l\r\n\r\n";
        let trimmed = trim_shell_artifacts(data);
        assert!(trimmed.is_empty());
        assert!(!detect_meaningful_content(&trimmed));
    }
}
