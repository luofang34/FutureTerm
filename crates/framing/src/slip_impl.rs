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
        match data[i] {
            SLIP_ESC => {
                i += 1;
                if i >= data.len() {
                    // Trailing ESC? Error.
                    return Err(());
                }
                match data[i] {
                    SLIP_ESC_END => out.push(SLIP_END),
                    SLIP_ESC_ESC => out.push(SLIP_ESC),
                    _ => return Err(()), // Invalid escape
                }
            },
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
    fn test_slip() {
        let mut framer = SlipFramer::new();
        // Encoded [0x01, 0xC0 (END in data)] -> [0x01, 0xDB, 0xDC]
        // Add framing END (0xC0)
        let input = vec![0x01, 0xDB, 0xDC, 0xC0];
        
        let frames = framer.push(&input, 100);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].bytes, vec![0x01, 0xC0]);
    }
}
