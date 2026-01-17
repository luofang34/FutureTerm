use serde::{Deserialize, Serialize};


pub mod transport;
pub use transport::{Transport, TransportError, SignalState, SerialConfig};

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

use std::borrow::Cow;

/// Lightweight value wrapper to avoid serde_json allocations for common types.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Value {
    Null,
    Bool(bool),
    I64(i64),
    F64(f64),
    String(String),
}

impl From<bool> for Value { fn from(v: bool) -> Self { Value::Bool(v) } }
impl From<i64> for Value { fn from(v: i64) -> Self { Value::I64(v) } }
impl From<i32> for Value { fn from(v: i32) -> Self { Value::I64(v as i64) } }
impl From<f64> for Value { fn from(v: f64) -> Self { Value::F64(v) } }
impl From<f32> for Value { fn from(v: f32) -> Self { Value::F64(v as f64) } }
impl From<String> for Value { fn from(v: String) -> Self { Value::String(v) } }
impl From<&str> for Value { fn from(v: &str) -> Self { Value::String(v.to_string()) } }
impl From<usize> for Value { fn from(v: usize) -> Self { Value::I64(v as i64) } }

/// A semantic interpretation of a Frame.
/// Optimized for low allocation frequency.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DecodedEvent {
    /// Timestamp inherited from the Frame.
    pub timestamp_us: u64,
    /// Protocol name (e.g., "NMEA", "MAVLink", "Hex").
    pub protocol: Cow<'static, str>,
    /// Short human-readable summary (e.g., "GPGGA: 3D Fix").
    pub summary: Cow<'static, str>,
    /// Structured key-value pairs for deep inspection.
    /// Using Vec instead of HashMap to avoid allocation for small number of fields.
    pub fields: Vec<(Cow<'static, str>, Value)>,
    /// How confident the decoder is (0.0 - 1.0).
    pub confidence: f32,
}

impl DecodedEvent {
    pub fn new(timestamp_us: u64, protocol: &'static str, summary: impl Into<Cow<'static, str>>) -> Self {
        Self {
            timestamp_us,
            protocol: Cow::Borrowed(protocol),
            summary: summary.into(),
            fields: Vec::with_capacity(8), // Pre-allocate small capacity
            confidence: 1.0,
        }
    }

    pub fn with_field<V: Into<Value>>(mut self, key: &'static str, value: V) -> Self {
        self.fields.push((Cow::Borrowed(key), value.into()));
        self
    }
    
    // Helper to get a field for tests
    pub fn get_field(&self, key: &str) -> Option<&Value> {
        self.fields.iter().find(|(k, _)| k == key).map(|(_, v)| v)
    }

    /// Resets the event data without deallocating the fields vector.
    pub fn clear(&mut self) {
        self.protocol = Cow::Borrowed("");
        self.summary = Cow::Borrowed("");
        self.fields.clear();
        self.confidence = 0.0;
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

    /// Attempt to parse a frame into an existing DecodedEvent buffer.
    /// Returns true if successful, false otherwise.
    /// 
    /// Detailed Instructions:
    /// Implementations should call `output.clear()` before populating.
    fn ingest_into(&mut self, frame: &Frame, output: &mut DecodedEvent) -> bool {
        if let Some(event) = self.ingest(frame) {
            *output = event;
            true
        } else {
            false
        }
    }

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
        assert_eq!(event.fields.iter().find(|(k, _)| k == "temperature").unwrap().1, Value::F64(25.5));
        assert_eq!(event.fields.iter().find(|(k, _)| k == "valid").unwrap().1, Value::Bool(true));
    }
}
