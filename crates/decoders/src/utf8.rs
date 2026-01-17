use core_types::{Frame, DecodedEvent, Decoder};

/// A passthrough decoder that converts frames to UTF-8 strings.
/// Essential for preserving ANSI escape codes for terminal emulation.
pub struct Utf8Decoder;

impl Utf8Decoder {
    pub fn new() -> Self {
        Self
    }
}

impl Decoder for Utf8Decoder {
    fn ingest(&mut self, frame: &Frame) -> Option<DecodedEvent> {
        // Simple passthrough: Bytes -> String (lossy)
        let text = String::from_utf8_lossy(&frame.bytes).to_string();
        
        Some(DecodedEvent::new(frame.timestamp_us, "UTF-8", text))
    }

    fn id(&self) -> &'static str {
        "utf8"
    }

    fn name(&self) -> &'static str {
        "UTF-8 Passthrough"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use core_types::Value;

    #[test]
    fn test_utf8_decoder() {
        let mut decoder = Utf8Decoder::new();
        let bytes = vec![0x48, 0x65, 0x6C, 0x6C, 0x6F]; // "Hello"
        let frame = Frame::new_rx(bytes, 1000);
        
        let event = decoder.ingest(&frame).expect("Utf8 decoder always succeeds");
        
        assert_eq!(event.protocol, "UTF-8");
        assert_eq!(event.summary, "Hello");
    }
    
    #[test]
    fn test_utf8_ansi() {
         let mut decoder = Utf8Decoder::new();
         // ESC [ 31 m (Red)
         let bytes = vec![0x1B, 0x5B, 0x33, 0x31, 0x6D]; 
         let frame = Frame::new_rx(bytes, 2000);
         
         let event = decoder.ingest(&frame).expect("Succeeds");
         assert_eq!(event.summary, "\x1B[31m");
    }
}
