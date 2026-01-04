use crate::Framer;
use core_types::Frame;

pub struct RawFramer;

impl RawFramer {
    pub fn new() -> Self {
        Self
    }
}

impl Framer for RawFramer {
    fn push(&mut self, bytes: &[u8], timestamp_us: u64) -> Vec<Frame> {
        if bytes.is_empty() {
            return vec![];
        }
        // Raw mode: every chunk is a frame. Logic is simple.
        vec![Frame::new_rx(bytes.to_vec(), timestamp_us)]
    }

    fn reset(&mut self) {
        // No state to reset
    }

    fn name(&self) -> &'static str {
        "Raw"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_raw_framer() {
        let mut framer = RawFramer::new();
        let frames = framer.push(&[0x01, 0x02], 100);
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].bytes, vec![0x01, 0x02]);
        assert_eq!(frames[0].timestamp_us, 100);
    }
}
