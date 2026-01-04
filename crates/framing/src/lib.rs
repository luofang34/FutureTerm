use core_types::Frame;

pub mod raw;
pub mod lines;
pub mod cobs_impl;
pub mod slip_impl;

/// Trait for converting a stream of bytes into discrete Frames.
pub trait Framer: Send {
    /// Ingest new bytes and return any complete frames found.
    /// 
    /// # Arguments
    /// * `bytes` - The new chunk of data read from transport.
    /// * `timestamp_us` - The timestamp associated with this chunk.
    fn push(&mut self, bytes: &[u8], timestamp_us: u64) -> Vec<Frame>;

    /// Reset internal state (e.g., clear buffers).
    fn reset(&mut self);

    /// Get the name of the framer.
    fn name(&self) -> &'static str;
}
