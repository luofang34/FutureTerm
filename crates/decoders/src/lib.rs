use core_types::{Frame, DecodedEvent};

/// Trait for converting Frames into structured Events.
pub trait Decoder: Send {
    /// Attempt to parse a frame.
    /// Returns None if the frame does not match the protocol validation.
    fn ingest(&mut self, frame: &Frame) -> Option<DecodedEvent>;

    /// Get default name.
    fn name(&self) -> &'static str;
}
