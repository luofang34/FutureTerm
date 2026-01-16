use core_types::{Frame, DecodedEvent, Decoder};
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
        let mut event = DecodedEvent::new(0, "", "");
        if self.ingest_into(frame, &mut event) {
            Some(event)
        } else {
            None
        }
    }

    fn ingest_into(&mut self, frame: &Frame, output: &mut DecodedEvent) -> bool {
        // 1. Convert frame bytes to string (NMEA is ASCII)
        let Ok(s) = std::str::from_utf8(&frame.bytes) else {
            return false;
        };
        
        // 2. Parse
        match self.parser.parse(s) {
            Ok(sentence_type) => {
                // Success!
                // Clear the output buffer first to ensure clean state
                output.clear();
                
                output.timestamp_us = frame.timestamp_us;
                output.protocol = std::borrow::Cow::Borrowed("NMEA");

                let type_str = match sentence_type {
                    nmea::SentenceType::GGA => "GGA",
                    nmea::SentenceType::RMC => "RMC",
                    nmea::SentenceType::GSA => "GSA",
                    nmea::SentenceType::GSV => "GSV",
                    nmea::SentenceType::VTG => "VTG",
                    nmea::SentenceType::GLL => "GLL",
                    nmea::SentenceType::TXT => "TXT",
                    _ => "OTHER", // Fallback if we don't want to format!
                };
                
                output.summary = std::borrow::Cow::Borrowed(type_str);
                output.fields.push((std::borrow::Cow::Borrowed("type"), type_str.into()));
                    
                // Add specific fields if GPGGA
                if let nmea::SentenceType::GGA = sentence_type {
                    if let Some(lat) = self.parser.latitude {
                        output.fields.push((std::borrow::Cow::Borrowed("latitude"), lat.into()));
                    }
                    if let Some(lon) = self.parser.longitude {
                        output.fields.push((std::borrow::Cow::Borrowed("longitude"), lon.into()));
                    }
                    if let Some(alt) = self.parser.altitude {
                        output.fields.push((std::borrow::Cow::Borrowed("altitude"), alt.into()));
                    }
                }
                
                output.confidence = 1.0;
                true
            },
            Err(_e) => {
                false
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
        assert!(event.fields.iter().any(|(k, _)| k == "latitude"));
        assert!(event.fields.iter().any(|(k, _)| k == "longitude"));
    }

    #[test]
    fn test_nmea_invalid() {
        let mut decoder = NmeaDecoder::new();
        let frame = Frame::new_rx(b"$GPGGA,BadChecksum*FF\r\n".to_vec(), 1000);
        let event = decoder.ingest(&frame);
        assert!(event.is_none());
    }
}
