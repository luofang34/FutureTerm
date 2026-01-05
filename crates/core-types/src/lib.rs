use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Represents the direction of data flow.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Channel {
    Rx, // Received from device
    Tx, // Sent to device
}

/// A raw chunk of logical data (e.g., a line, a packet).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Frame {
    /// The raw bytes comprising this frame.
    pub bytes: Vec<u8>,
    /// Timestamp in microseconds (relative to session start or epoch).
    pub timestamp_us: u64,
    /// Direction of the frame.
    pub channel: Channel,
}

impl Frame {
    pub fn new_rx(bytes: Vec<u8>, timestamp_us: u64) -> Self {
        Self {
            bytes,
            timestamp_us,
            channel: Channel::Rx,
        }
    }

    pub fn new_tx(bytes: Vec<u8>, timestamp_us: u64) -> Self {
        Self {
            bytes,
            timestamp_us,
            channel: Channel::Tx,
        }
    }
}

/// A semantic interpretation of a Frame.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecodedEvent {
    /// Timestamp inherited from the Frame.
    pub timestamp_us: u64,
    /// Protocol name (e.g., "NMEA", "MAVLink", "Hex").
    pub protocol: String,
    /// Short human-readable summary (e.g., "GPGGA: 3D Fix").
    pub summary: String,
    /// Structured key-value pairs for deep inspection.
    pub fields: HashMap<String, serde_json::Value>,
    /// How confident the decoder is (0.0 - 1.0).
    /// Useful for heuristic detection.
    pub confidence: f32,
}

impl DecodedEvent {
    pub fn new(timestamp_us: u64, protocol: &str, summary: &str) -> Self {
        Self {
            timestamp_us,
            protocol: protocol.to_string(),
            summary: summary.to_string(),
            fields: HashMap::new(),
            confidence: 1.0,
        }
    }

    pub fn with_field<V: Serialize>(mut self, key: &str, value: V) -> Self {
        if let Ok(json_val) = serde_json::to_value(value) {
            self.fields.insert(key.to_string(), json_val);
        }
        self
    }
}

/// Trait for converting Frames into structured Events.
/// This is intended to be implemented by plugins (NMEA, MAVLink, etc.).
pub trait Decoder: Send {
    /// Attempt to parse a frame.
    /// Returns None if the frame does not match the protocol validation.
    /// 
    /// # Arguments
    /// * `frame` - The input frame (contains bytes, timestamp, channel).
    fn ingest(&mut self, frame: &Frame) -> Option<DecodedEvent>;

    /// Get the unique name of this decoder (e.g., "NMEA", "Hex").
    fn id(&self) -> &'static str;

    /// Get a human-readable name (e.g., "NMEA 0183").
    fn name(&self) -> &'static str;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_frame_serialization() {
        let frame = Frame::new_rx(vec![0x01, 0x02, 0x03], 1000);
        let json = serde_json::to_string(&frame).unwrap();
        let deserialized: Frame = serde_json::from_str(&json).unwrap();
        assert_eq!(frame, deserialized);
    }

    #[test]
    fn test_decoded_event_builder() {
        let event = DecodedEvent::new(123456, "TEST", "Test Event")
            .with_field("temperature", 25.5)
            .with_field("valid", true);
        
        assert_eq!(event.protocol, "TEST");
        assert_eq!(event.fields.get("temperature").unwrap().as_f64(), Some(25.5));
        assert_eq!(event.fields.get("valid").unwrap().as_bool(), Some(true));
    }
}
