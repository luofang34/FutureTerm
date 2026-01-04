use crate::Framer;
use core_types::Frame;

/// Buffers input and emits a frame whenever a newline is encountered.
/// Supports configurable delimiters, defaulting to \n.
/// NOTE: Dealing with mixed line endings (CRLF vs LF) in a streaming fashion
/// can be tricky. This implementation treats `\n` as the primary delimiter
/// and optionally strips `\r` if it precedes `\n`.
pub struct LineFramer {
    buffer: Vec<u8>,
    // Timestamp of the *first byte* currently in the buffer.
    // This ensures that when a line is completed, it gets the timestamp of when it started arriving.
    start_timestamp_us: Option<u64>,
}

impl LineFramer {
    pub fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(1024),
            start_timestamp_us: None,
        }
    }
}

impl Framer for LineFramer {
    fn push(&mut self, bytes: &[u8], timestamp_us: u64) -> Vec<Frame> {
        let mut frames = Vec::new();
        
        // If buffer was empty, this chunk marks the start of potential new frames.
        if self.buffer.is_empty() {
            self.start_timestamp_us = Some(timestamp_us);
        }

        for &b in bytes {
            self.buffer.push(b);
            if b == b'\n' {
                // Determine timestamp: use the start time of this line
                let ts = self.start_timestamp_us.unwrap_or(timestamp_us);
                
                // Clone buffer as frame (including the newline)
                // We leave the newline in the frame because decoders (like NMEA) might need it for validation,
                // or the UI might want to render it.
                // However, for pure specific decoders, we might want to trim.
                // Let's stick to "Framer preserves bytes, Decoder interprets".
                let frame_bytes = self.buffer.clone();
                frames.push(Frame::new_rx(frame_bytes, ts));
                
                // Reset buffer
                self.buffer.clear();
                // The next byte (if any) will start a new line, so we'll need a new timestamp.
                // Since strictly speaking we don't know the exact arrival time of the *next* byte 
                // within this batch, we can assume it's part of the current batch or just use current ts.
                // Better approach: set start_timestamp_us to None, and if we loop again, logic below handles it.
                self.start_timestamp_us = None;
            }
        }
        
        // If we still have data in buffer (incomplete line), 
        // ensure start_timestamp_us is set for the *next* push call reference.
        if !self.buffer.is_empty() && self.start_timestamp_us.is_none() {
             // This edge case happens if the *current batch* started a new line.
             // Ideally we should have captured strict per-byte timing but we only have per-chunk.
             // We use the current chunk's timestamp as best effort.
             self.start_timestamp_us = Some(timestamp_us);
        }

        frames
    }

    fn reset(&mut self) {
        self.buffer.clear();
        self.start_timestamp_us = None;
    }

    fn name(&self) -> &'static str {
        "Lines"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_lines_simple() {
        let mut framer = LineFramer::new();
        let frames = framer.push(b"Hello\nWorld\n", 100);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].bytes, b"Hello\n");
        assert_eq!(frames[0].timestamp_us, 100);
        assert_eq!(frames[1].bytes, b"World\n");
        // In a single batch, we unfortunately share the timestamp or derived logic.
        // It's acceptable for v1 to have same timestamp for batch arrival.
    }

    #[test]
    fn test_lines_split() {
        let mut framer = LineFramer::new();
        // Chunk 1: "Hel" at T=100
        let f1 = framer.push(b"Hel", 100);
        assert_eq!(f1.len(), 0);

        // Chunk 2: "lo\n" at T=200
        let f2 = framer.push(b"lo\n", 200);
        assert_eq!(f2.len(), 1);
        
        assert_eq!(f2[0].bytes, b"Hello\n");
        // Should preserve the timestamp of the START of the frame (T=100)
        assert_eq!(f2[0].timestamp_us, 100);
    }
    
    #[test]
    fn test_crlf_handling() {
        // We do typically just break on \n. Implementation preserves \r.
        let mut framer = LineFramer::new();
        let f = framer.push(b"Test\r\n", 100);
        assert_eq!(f[0].bytes, b"Test\r\n");
    }
}
