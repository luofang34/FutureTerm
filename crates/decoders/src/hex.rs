use crate::Decoder;
use core_types::{Frame, DecodedEvent};

/// A universal decoder that converts ANY frame into a Hex Dump event.
/// Useful for "Hex View" mode or debugging unknown protocols.
pub struct HexDecoder;

impl HexDecoder {
    pub fn new() -> Self {
        Self
    }
}

impl Decoder for HexDecoder {
    fn ingest(&mut self, frame: &Frame) -> Option<DecodedEvent> {
        // HexDecoder accepts everything.
        // It produces an event with "Hex" protocol.
        
        // Format: "0A 1B FF ..."
        let hex_str = frame.bytes.iter()
            .map(|b| format!("{:02X}", b))
            .collect::<Vec<String>>()
            .join(" ");

        // ASCII representation for side-by-side view
        // Replace non-printable with '.'
        let ascii_str: String = frame.bytes.iter()
            .map(|&b| if b >= 32 && b <= 126 { b as char } else { '.' })
            .collect();

        // Create the event
        // Summary is just the hex string (truncated if too long, UI handles truncation usually)
        let summary = if hex_str.len() > 50 {
            format!("{}...", &hex_str[0..47])
        } else {
            hex_str.clone()
        };

        Some(DecodedEvent::new(frame.timestamp_us, "Hex", &summary)
            .with_field("hex", hex_str)
            .with_field("ascii", ascii_str)
            .with_field("len", frame.bytes.len()))
    }

    fn id(&self) -> &'static str {
        "hex"
    }

    fn name(&self) -> &'static str {
        "Hex Inspector"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_hex_decoder() {
        let mut decoder = HexDecoder::new();
        let frame = Frame::new_rx(vec![0x41, 0x42, 0x00, 0xFF], 1000);
        
        let event = decoder.ingest(&frame).expect("Hex decoder always succeeds");
        
        assert_eq!(event.protocol, "Hex");
        assert_eq!(event.fields.get("hex").unwrap().as_str().unwrap(), "41 42 00 FF");
        assert_eq!(event.fields.get("ascii").unwrap().as_str().unwrap(), "AB..");
        assert_eq!(event.fields.get("len").unwrap().as_u64().unwrap(), 4);
    }
}
