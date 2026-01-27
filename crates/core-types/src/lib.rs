use serde::{Deserialize, Serialize};
use std::str::FromStr;

pub mod transport;
pub use transport::{SerialConfig, SignalState, Transport, TransportError};

/// Decoder protocol type - determines how raw bytes are interpreted
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum DecoderId {
    /// UTF-8 text decoder (default for terminal view)
    #[default]
    Utf8,
    /// Raw hexadecimal decoder
    Hex,
    /// MAVLink protocol decoder
    Mavlink,
}

impl DecoderId {
    /// Convert to string ID for backward compatibility with worker protocol
    pub fn as_str(&self) -> &'static str {
        match self {
            DecoderId::Utf8 => "utf8",
            DecoderId::Hex => "hex",
            DecoderId::Mavlink => "mavlink",
        }
    }
}

impl FromStr for DecoderId {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "utf8" => Ok(DecoderId::Utf8),
            "hex" => Ok(DecoderId::Hex),
            "mavlink" => Ok(DecoderId::Mavlink),
            _ => Err(format!("Invalid decoder ID: {}", s)),
        }
    }
}

/// Framing protocol type - determines how byte stream is split into frames
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum FramerId {
    /// Line-based framing (newline delimited)
    Lines,
    /// Raw byte stream (no framing)
    #[default]
    Raw,
    /// Consistent Overhead Byte Stuffing
    Cobs,
    /// Serial Line Internet Protocol
    Slip,
}

impl FramerId {
    /// Convert to string ID for backward compatibility with worker protocol
    pub fn as_str(&self) -> &'static str {
        match self {
            FramerId::Lines => "lines",
            FramerId::Raw => "raw",
            FramerId::Cobs => "cobs",
            FramerId::Slip => "slip",
        }
    }
}

impl FromStr for FramerId {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "lines" => Ok(FramerId::Lines),
            "raw" => Ok(FramerId::Raw),
            "cobs" => Ok(FramerId::Cobs),
            "slip" => Ok(FramerId::Slip),
            _ => Err(format!("Invalid framer ID: {}", s)),
        }
    }
}

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

/// A raw event stored in the unified append-only log.
/// This is the persistent storage format for all received/sent data,
/// from which each decoder view derives its own interpretation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RawEvent {
    /// Timestamp in microseconds (relative to session start or epoch).
    pub timestamp_us: u64,
    /// Direction of data flow (RX from device, TX to device).
    pub channel: Channel,
    /// The raw bytes.
    pub bytes: Vec<u8>,
}

impl RawEvent {
    pub fn new(timestamp_us: u64, channel: Channel, bytes: Vec<u8>) -> Self {
        Self {
            timestamp_us,
            channel,
            bytes,
        }
    }

    pub fn from_frame(frame: &Frame) -> Self {
        Self {
            timestamp_us: frame.timestamp_us,
            channel: frame.channel,
            bytes: frame.bytes.clone(),
        }
    }

    pub fn byte_size(&self) -> usize {
        self.bytes.len()
    }
}

/// Represents the source of a selection operation (which view initiated it).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SelectionSource {
    Terminal,
    HexView,
    // MAVLinkView,  // Reserved for future implementation
}

/// Represents a selection range across views, based on byte offsets in raw_log.
/// This enables cross-view selection synchronization (e.g., Terminal ↔ Hex).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SelectionRange {
    /// Start byte offset (relative to raw_log's cumulative position)
    pub start_byte_offset: usize,

    /// End byte offset (exclusive)
    pub end_byte_offset: usize,

    /// Timestamp range for synchronization and validation
    pub timestamp_start_us: u64,
    pub timestamp_end_us: u64,

    /// Source view identifier
    pub source_view: SelectionSource,
}

impl SelectionRange {
    pub fn new(
        start_byte_offset: usize,
        end_byte_offset: usize,
        timestamp_start_us: u64,
        timestamp_end_us: u64,
        source_view: SelectionSource,
    ) -> Self {
        Self {
            start_byte_offset,
            end_byte_offset,
            timestamp_start_us,
            timestamp_end_us,
            source_view,
        }
    }

    /// Returns the number of bytes in this selection
    pub fn byte_len(&self) -> usize {
        self.end_byte_offset.saturating_sub(self.start_byte_offset)
    }

    /// Check if a byte offset is within this selection range
    pub fn contains_offset(&self, offset: usize) -> bool {
        offset >= self.start_byte_offset && offset < self.end_byte_offset
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

impl From<bool> for Value {
    fn from(v: bool) -> Self {
        Value::Bool(v)
    }
}
impl From<i64> for Value {
    fn from(v: i64) -> Self {
        Value::I64(v)
    }
}
impl From<i32> for Value {
    fn from(v: i32) -> Self {
        Value::I64(v as i64)
    }
}
impl From<f64> for Value {
    fn from(v: f64) -> Self {
        Value::F64(v)
    }
}
impl From<f32> for Value {
    fn from(v: f32) -> Self {
        Value::F64(v as f64)
    }
}
impl From<String> for Value {
    fn from(v: String) -> Self {
        Value::String(v)
    }
}
impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Value::String(v.to_string())
    }
}
impl From<usize> for Value {
    fn from(v: usize) -> Self {
        Value::I64(v as i64)
    }
}

impl std::fmt::Display for Value {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Value::Null => write!(f, "null"),
            Value::Bool(b) => write!(f, "{}", b),
            Value::I64(n) => write!(f, "{}", n),
            Value::F64(n) => write!(f, "{}", n),
            Value::String(s) => write!(f, "{}", s),
        }
    }
}

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
    pub fn new(
        timestamp_us: u64,
        protocol: &'static str,
        summary: impl Into<Cow<'static, str>>,
    ) -> Self {
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
    /// Appends generated events to the `results` vector.
    ///
    /// # Arguments
    /// * `frame` - The input frame (contains bytes, timestamp, channel).
    /// * `results` - Mutable vector to append decoded events to.
    fn ingest(&mut self, frame: &Frame, results: &mut Vec<DecodedEvent>);

    /// Get the unique name of this decoder (e.g., "NMEA", "Hex").
    fn id(&self) -> &'static str;

    /// Get a human-readable name (e.g., "NMEA 0183").
    fn name(&self) -> &'static str;
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::indexing_slicing)]
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
        assert_eq!(
            event
                .fields
                .iter()
                .find(|(k, _)| k == "temperature")
                .unwrap()
                .1,
            Value::F64(25.5)
        );
        assert_eq!(
            event.fields.iter().find(|(k, _)| k == "valid").unwrap().1,
            Value::Bool(true)
        );
    }

    #[test]
    fn test_decoder_id_conversions() {
        use std::str::FromStr;

        assert_eq!(DecoderId::Utf8.as_str(), "utf8");
        assert_eq!(DecoderId::Hex.as_str(), "hex");
        assert_eq!(DecoderId::Mavlink.as_str(), "mavlink");

        assert_eq!(DecoderId::from_str("utf8"), Ok(DecoderId::Utf8));
        assert_eq!(DecoderId::from_str("hex"), Ok(DecoderId::Hex));
        assert_eq!(DecoderId::from_str("mavlink"), Ok(DecoderId::Mavlink));
        assert!(DecoderId::from_str("invalid").is_err());

        assert_eq!(DecoderId::default(), DecoderId::Utf8);
    }

    #[test]
    fn test_framer_id_conversions() {
        use std::str::FromStr;

        assert_eq!(FramerId::Lines.as_str(), "lines");
        assert_eq!(FramerId::Raw.as_str(), "raw");
        assert_eq!(FramerId::Cobs.as_str(), "cobs");
        assert_eq!(FramerId::Slip.as_str(), "slip");

        assert_eq!(FramerId::from_str("lines"), Ok(FramerId::Lines));
        assert_eq!(FramerId::from_str("raw"), Ok(FramerId::Raw));
        assert_eq!(FramerId::from_str("cobs"), Ok(FramerId::Cobs));
        assert_eq!(FramerId::from_str("slip"), Ok(FramerId::Slip));
        assert!(FramerId::from_str("invalid").is_err());

        assert_eq!(FramerId::default(), FramerId::Raw);
    }

    #[test]
    fn test_decoder_id_compile_time_exhaustiveness() {
        // This test ensures we handle all decoder types at compile time
        let decoder = DecoderId::Utf8;
        match decoder {
            DecoderId::Utf8 => {}
            DecoderId::Hex => {}
            DecoderId::Mavlink => {} // If a new variant is added, this will fail to compile
        }
    }

    #[test]
    fn test_framer_id_compile_time_exhaustiveness() {
        // This test ensures we handle all framer types at compile time
        let framer = FramerId::Raw;
        match framer {
            FramerId::Lines => {}
            FramerId::Raw => {}
            FramerId::Cobs => {}
            FramerId::Slip => {} // If a new variant is added, this will fail to compile
        }
    }
}
