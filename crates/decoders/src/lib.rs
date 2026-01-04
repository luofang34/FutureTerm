use core_types::{Frame, DecodedEvent};

pub mod hex;

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
