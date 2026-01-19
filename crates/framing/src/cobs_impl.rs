use crate::Framer;
use core_types::Frame;

pub struct CobsFramer {
    buffer: Vec<u8>,
    timestamp_us: Option<u64>,
}

impl CobsFramer {
    pub fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(1024),
            timestamp_us: None,
        }
    }
}

impl Default for CobsFramer {
    fn default() -> Self {
        Self::new()
    }
}

impl Framer for CobsFramer {
    fn push(&mut self, bytes: &[u8], timestamp_us: u64) -> Vec<Frame> {
        let mut frames = Vec::new();

        if self.buffer.is_empty() {
            self.timestamp_us = Some(timestamp_us);
        }

        for &b in bytes {
            // COBS uses 0x00 as delimiter
            if b == 0x00 {
                if !self.buffer.is_empty() {
                    // Start of frame timestamp
                    let ts = self.timestamp_us.unwrap_or(timestamp_us);

                    // Decode
                    if let Ok(decoded) = cobs::decode_vec(&self.buffer) {
                        frames.push(Frame::new_rx(decoded, ts));
                    } else {
                        // Bad frame (invalid COBS), drop or handle error?
                        // For now silent drop is standard for stream recovery.
                    }
                    self.buffer.clear();
                }
                // Reset timestamp for next frame
                self.timestamp_us = None;
            } else {
                self.buffer.push(b);
            }
        }

        // If buffer has data, ensure we have a timestamp for the next chunk
        if !self.buffer.is_empty() && self.timestamp_us.is_none() {
            self.timestamp_us = Some(timestamp_us);
        }

        frames
    }

    fn reset(&mut self) {
        self.buffer.clear();
        self.timestamp_us = None;
    }

    fn name(&self) -> &'static str {
        "COBS"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cobs_basic() {
        let mut framer = CobsFramer::new();
        // Encoded "Hello"
        let encoded = cobs::encode_vec(b"Hello"); // Should be [0x06, 'H', ..., 'o'] no zero
                                                  // delimiter is 0x00
        let mut input = encoded.clone();
        input.push(0x00);

        let frames = framer.push(&input, 100);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].bytes, b"Hello");
    }

    #[test]
    fn test_cobs_partial() {
        let mut framer = CobsFramer::new();
        let encoded = cobs::encode_vec(b"World");

        // Push first part
        framer.push(&encoded[0..2], 100);

        // Push second part + delimiter
        let mut part2 = encoded[2..].to_vec();
        part2.push(0x00);

        let frames = framer.push(&part2, 200);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].bytes, b"World");
        // Timestamp should be start time
        assert_eq!(frames[0].timestamp_us, 100);
    }
}
