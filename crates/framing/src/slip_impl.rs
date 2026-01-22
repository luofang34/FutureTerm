use crate::Framer;
use core_types::Frame;

pub struct SlipFramer {
    buffer: Vec<u8>,
    timestamp_us: Option<u64>,
}

impl SlipFramer {
    pub fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(1024),
            timestamp_us: None,
        }
    }
}

impl Default for SlipFramer {
    fn default() -> Self {
        Self::new()
    }
}

// RFC 1055 constants
const SLIP_END: u8 = 0xC0;
const SLIP_ESC: u8 = 0xDB;
const SLIP_ESC_END: u8 = 0xDC;
const SLIP_ESC_ESC: u8 = 0xDD;

impl Framer for SlipFramer {
    fn push(&mut self, bytes: &[u8], timestamp_us: u64) -> Vec<Frame> {
        let mut frames = Vec::new();

        if self.buffer.is_empty() {
            self.timestamp_us = Some(timestamp_us);
        }

        for &b in bytes {
            if b == SLIP_END {
                if !self.buffer.is_empty() {
                    let ts = self.timestamp_us.unwrap_or(timestamp_us);

                    // Decode SLIP manually to avoid complex dependency if not needed,
                    // but slip-codec is fine. Let's do manual for control/no-alloc.
                    // Actually, let's just use a helper function to keep this clear.
                    if let Ok(decoded) = decode_slip(&self.buffer) {
                        frames.push(Frame::new_rx(decoded, ts));
                    }
                    self.buffer.clear();
                }
                self.timestamp_us = None;
            } else {
                self.buffer.push(b);
            }
        }

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
        "SLIP"
    }
}

fn decode_slip(data: &[u8]) -> Result<Vec<u8>, ()> {
    let mut out = Vec::with_capacity(data.len());
    let mut i = 0;
    while i < data.len() {
        let Some(&byte) = data.get(i) else {
            break;
        };
        match byte {
            SLIP_ESC => {
                i += 1;
                let Some(&escaped_byte) = data.get(i) else {
                    // Trailing ESC? Error.
                    return Err(());
                };
                match escaped_byte {
                    SLIP_ESC_END => out.push(SLIP_END),
                    SLIP_ESC_ESC => out.push(SLIP_ESC),
                    _ => return Err(()), // Invalid escape
                }
            }
            x => out.push(x),
        }
        i += 1;
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_slip_basic() {
        let mut framer = SlipFramer::new();
        // Encoded [0x01, 0xC0 (END in data)] -> [0x01, 0xDB, 0xDC]
        // Add framing END (0xC0)
        let input = vec![0x01, 0xDB, 0xDC, 0xC0];

        let frames = framer.push(&input, 100);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].bytes, vec![0x01, 0xC0]);
    }

    #[test]
    fn test_slip_escape_esc() {
        let mut framer = SlipFramer::new();
        // Data containing ESC byte (0xDB) -> encoded as [0xDB, 0xDD]
        let input = vec![0x01, 0xDB, 0xDD, 0xC0];

        let frames = framer.push(&input, 100);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].bytes, vec![0x01, 0xDB]);
    }

    #[test]
    fn test_slip_multiple_frames() {
        let mut framer = SlipFramer::new();
        // Two frames: [0x01] and [0x02]
        let input = vec![0x01, 0xC0, 0x02, 0xC0];

        let frames = framer.push(&input, 100);
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[0].bytes, vec![0x01]);
        assert_eq!(frames[1].bytes, vec![0x02]);
    }

    #[test]
    fn test_slip_split_frame() {
        let mut framer = SlipFramer::new();
        // First chunk: partial frame
        let f1 = framer.push(&[0x01, 0x02], 100);
        assert_eq!(f1.len(), 0);

        // Second chunk: rest of frame + delimiter
        let f2 = framer.push(&[0x03, 0xC0], 200);
        assert_eq!(f2.len(), 1);
        assert_eq!(f2[0].bytes, vec![0x01, 0x02, 0x03]);
        assert_eq!(f2[0].timestamp_us, 100); // Start timestamp
    }

    #[test]
    fn test_slip_empty_frame() {
        let mut framer = SlipFramer::new();
        // Just delimiter -> empty frame (should be ignored)
        let frames = framer.push(&[0xC0], 100);
        assert_eq!(frames.len(), 0);
    }

    #[test]
    fn test_slip_reset() {
        let mut framer = SlipFramer::new();
        // Push partial
        framer.push(&[0x01, 0x02], 100);
        // Reset
        framer.reset();
        // Push new complete frame
        let frames = framer.push(&[0x03, 0xC0], 200);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].bytes, vec![0x03]);
        assert_eq!(frames[0].timestamp_us, 200);
    }
}
