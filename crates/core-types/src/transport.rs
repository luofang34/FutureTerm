use thiserror::Error;

#[derive(Error, Debug, PartialEq)]
pub enum TransportError {
    #[error("IO Error: {0}")]
    Io(String),
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),
    #[error("Not connected")]
    NotConnected,
    #[error("Already open")]
    AlreadyOpen,
    #[error("Invalid state: {0}")]
    InvalidState(String),
    #[error("Other: {0}")]
    Other(String),
}

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct SignalState {
    pub dtr: bool,
    pub rts: bool,
    pub dcd: bool, // Data Carrier Detect
    pub dsr: bool, // Data Set Ready
    pub ri: bool,  // Ring Indicator
    pub cts: bool, // Clear To Send
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SerialConfig {
    pub baud_rate: u32,
    pub data_bits: u8,        // 7 or 8
    pub flow_control: String, // "none" or "hardware"
    pub parity: String,       // "none", "even", "odd"
    pub stop_bits: u8,        // 1 or 2
}

impl Default for SerialConfig {
    fn default() -> Self {
        Self {
            baud_rate: 115200,
            data_bits: 8,
            flow_control: "none".into(),
            parity: "none".into(),
            stop_bits: 1,
        }
    }
}
#[allow(async_fn_in_trait)]
pub trait Transport: Send + Sync {
    /// Check if the transport is currently open.
    fn is_open(&self) -> bool;

    /// Read a chunk of bytes.
    /// Returns (data, timestamp_in_microseconds).
    async fn read_chunk(&self) -> Result<(Vec<u8>, u64), TransportError>;

    /// Write bytes to the transport.
    async fn write(&self, data: &[u8]) -> Result<(), TransportError>;

    /// Control DTR and RTS signals.
    async fn set_signals(&self, dtr: bool, rts: bool) -> Result<(), TransportError>;

    /// Read signal state (CTS, DSR, DCD, RI).
    async fn get_signals(&self) -> Result<SignalState, TransportError>;

    /// Close the connection.
    async fn close(&mut self) -> Result<(), TransportError>;
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn test_transport_error_already_open() {
        let err = TransportError::AlreadyOpen;
        assert_eq!(err.to_string(), "Already open");
        assert_eq!(err, TransportError::AlreadyOpen);
    }

    #[test]
    fn test_transport_error_invalid_state() {
        let err = TransportError::InvalidState("port locked".into());
        assert_eq!(err.to_string(), "Invalid state: port locked");
    }

    #[test]
    fn test_transport_error_pattern_matching() {
        // Verify retriable errors
        let retriable_errors = vec![
            TransportError::AlreadyOpen,
            TransportError::InvalidState("test".into()),
        ];

        for err in retriable_errors {
            match err {
                TransportError::AlreadyOpen | TransportError::InvalidState(_) => {
                    // Expected: these should trigger retries
                }
                _ => panic!("Expected retriable error, got: {}", err),
            }
        }

        // Verify non-retriable errors
        let non_retriable_errors = vec![
            TransportError::NotConnected,
            TransportError::Io("timeout".into()),
        ];

        for err in non_retriable_errors {
            match err {
                TransportError::AlreadyOpen | TransportError::InvalidState(_) => {
                    panic!("Expected non-retriable error, got: {}", err);
                }
                _ => {
                    // Expected: these should fail immediately
                }
            }
        }
    }

    #[test]
    fn test_signal_state_default() {
        let signals = SignalState::default();
        assert!(!signals.dtr);
        assert!(!signals.rts);
        assert!(!signals.dcd);
        assert!(!signals.dsr);
        assert!(!signals.ri);
        assert!(!signals.cts);
    }

    #[test]
    fn test_serial_config_default() {
        let config = SerialConfig::default();
        assert_eq!(config.baud_rate, 115200);
        assert_eq!(config.data_bits, 8);
        assert_eq!(config.flow_control, "none");
        assert_eq!(config.parity, "none");
        assert_eq!(config.stop_bits, 1);
    }
}
