use decoders::Decoder;
use core_types::{Frame, DecodedEvent};
use nmea::Nmea;

pub struct NmeaDecoder {
    parser: Nmea,
}

impl NmeaDecoder {
    pub fn new() -> Self {
        Self {
            parser: Nmea::default(),
        }
    }
}

impl Decoder for NmeaDecoder {
    fn ingest(&mut self, frame: &Frame) -> Option<DecodedEvent> {
        // 1. Convert frame bytes to string (NMEA is ASCII)
        let s = std::str::from_utf8(&frame.bytes).ok()?;
        
        // 2. Parse
        // Note: `nmea` crate's `parse` method typically expects the full sentence including $ and checksum.
        // It updates internal state.
        match self.parser.parse(s) {
            Ok(sentence_type) => {
                // Success!
                // We construct an event based on the parsed data.
                // For now, simpler summary: just the sentence type and key info.
                
                // Note: The `nmea` parser maintains state (fix, etc.).
                // We might want to snapshot that state into the event.
                
                let summary = format!("NMEA {:?}", sentence_type);
                let mut event = DecodedEvent::new(frame.timestamp_us, "NMEA", &summary)
                    .with_field("type", format!("{:?}", sentence_type));
                    
                // Add specific fields if GPGGA (Global Positioning System Fix Data)
                // Just as an example of structured data extraction
                if let nmea::SentenceType::GGA = sentence_type {
                    if let Some(lat) = self.parser.latitude {
                        event = event.with_field("latitude", lat);
                    }
                    if let Some(lon) = self.parser.longitude {
                        event = event.with_field("longitude", lon);
                    }
                    if let Some(alt) = self.parser.altitude {
                        event = event.with_field("altitude", alt);
                    }
                }

                Some(event)
            },
            Err(_e) => {
                // Parse failed (CRC error, invalid format, etc.)
                // Return None -> This frame is not valid NMEA (or at least this decoder thinks so)
                None
            }
        }
    }

    fn id(&self) -> &'static str {
        "nmea"
    }

    fn name(&self) -> &'static str {
        "NMEA 0183"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_nmea_decoder_gga() {
        let mut decoder = NmeaDecoder::new();
        // Standard GPGGA example with checksum
        let raw = b"$GPGGA,092750.000,5321.6802,N,00630.3372,W,1,8,1.03,61.7,M,55.2,M,,*76\r\n";
        let frame = Frame::new_rx(raw.to_vec(), 1000);
        
        let event = decoder.ingest(&frame).expect("Should parse GPGGA");
        
        assert_eq!(event.protocol, "NMEA");
        // Check dynamic fields (simplified check, real usage would check lat/lon values)
        assert!(event.fields.contains_key("latitude"));
        assert!(event.fields.contains_key("longitude"));
    }

    #[test]
    fn test_nmea_invalid() {
        let mut decoder = NmeaDecoder::new();
        let frame = Frame::new_rx(b"$GPGGA,BadChecksum*FF\r\n".to_vec(), 1000);
        let event = decoder.ingest(&frame);
        assert!(event.is_none());
    }
}
