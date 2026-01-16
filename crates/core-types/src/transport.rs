use thiserror::Error;

#[derive(Error, Debug)]
pub enum TransportError {
    #[error("IO Error: {0}")]
    Io(String),
    #[error("Connection failed: {0}")]
    ConnectionFailed(String),
    #[error("Not connected")]
    NotConnected,
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

/// A generic async transport layer (Serial, WebSocket, TCP, etc.)
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
