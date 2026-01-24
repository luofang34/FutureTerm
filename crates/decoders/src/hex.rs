use core_types::{DecodedEvent, Decoder, Frame};

/// A universal decoder that converts ANY frame into a Hex Dump event.
/// Useful for "Hex View" mode or debugging unknown protocols.
pub struct HexDecoder;

impl HexDecoder {
    pub fn new() -> Self {
        Self
    }
}

impl Default for HexDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder for HexDecoder {
    fn ingest(&mut self, frame: &Frame, results: &mut Vec<DecodedEvent>) {
        // HexDecoder accepts everything.
        // It produces an event with "Hex" protocol.

        // Format: "0A 1B FF ..."
        let hex_str = frame
            .bytes
            .iter()
            .map(|b| format!("{:02X}", b))
            .collect::<Vec<String>>()
            .join(" ");

        // ASCII representation for side-by-side view
        // Replace non-printable with '.'
        let ascii_str: String = frame
            .bytes
            .iter()
            .map(|&b| {
                if (32..=126).contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect();

        // Create the event
        // Summary is just the hex string (truncated if too long, UI handles truncation usually)
        let summary = if hex_str.len() > 50 {
            format!("{}...", &hex_str[0..47])
        } else {
            hex_str.clone()
        };

        results.push(
            DecodedEvent::new(frame.timestamp_us, "Hex", summary)
                .with_field("hex", hex_str)
                .with_field("ascii", ascii_str)
                .with_field("len", frame.bytes.len()),
        );
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
    use core_types::Value;

    #[test]
    fn test_hex_decoder() {
        let mut decoder = HexDecoder::new();
        let frame = Frame::new_rx(vec![0x41, 0x42, 0x00, 0xFF], 1000);

        let mut results = Vec::new();
        decoder.ingest(&frame, &mut results);
        let event = results.first().expect("Hex decoder always succeeds");

        assert_eq!(event.protocol, "Hex");

        match event.get_field("hex") {
            Some(Value::String(s)) => assert_eq!(s, "41 42 00 FF"),
            _ => panic!("Expected hex string field"),
        }

        match event.get_field("ascii") {
            Some(Value::String(s)) => assert_eq!(s, "AB.."),
            _ => panic!("Expected ascii string field"),
        }

        match event.get_field("len") {
            Some(Value::I64(v)) => assert_eq!(*v, 4),
            _ => panic!("Expected len integer field"),
        }
    }

    #[test]
    fn test_hex_decoder_empty() {
        let mut decoder = HexDecoder::new();
        let frame = Frame::new_rx(vec![], 1000);

        let mut results = Vec::new();
        decoder.ingest(&frame, &mut results);
        let event = results.first().expect("Hex decoder always succeeds");

        match event.get_field("len") {
            Some(Value::I64(v)) => assert_eq!(*v, 0),
            _ => panic!("Expected len integer field"),
        }
    }

    #[test]
    fn test_hex_decoder_long_truncation() {
        let mut decoder = HexDecoder::new();
        // Create a long frame (> 50 chars in hex representation)
        let frame = Frame::new_rx(vec![0xAA; 50], 1000);

        let mut results = Vec::new();
        decoder.ingest(&frame, &mut results);
        let event = results.first().expect("Hex decoder always succeeds");

        // Summary should be truncated
        assert!(event.summary.ends_with("..."));
        assert!(event.summary.len() <= 50);
    }

    #[test]
    fn test_hex_decoder_all_printable() {
        let mut decoder = HexDecoder::new();
        let frame = Frame::new_rx(b"Hello".to_vec(), 1000);

        let mut results = Vec::new();
        decoder.ingest(&frame, &mut results);
        let event = results.first().expect("Hex decoder always succeeds");

        match event.get_field("ascii") {
            Some(Value::String(s)) => assert_eq!(s, "Hello"),
            _ => panic!("Expected ascii string field"),
        }
    }
}
