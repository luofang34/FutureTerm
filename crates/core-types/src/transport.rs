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

/// A generic async transport layer (Serial, WebSocket, TCP, etc.)
/// 
/// Note: We use `async_trait` compatible signatures directly if on Rust 1.75+,
/// or use the `async_trait` macro if we need to support older rust or dynamic dispatch easily.
/// Given user prompt syntax, standard async fn in trait is likely meant.
/// However, for object safety (Box<dyn Transport>), native async fn in traits have caveats (Send bounds etc).
/// For now, we define it as standard async fn.
#[allow(async_fn_in_trait)]
pub trait Transport: Send + Sync {
    /// Read a chunk of bytes.
    /// Returns (data, timestamp).
    async fn read_chunk(&self) -> Result<(Vec<u8>, f64), TransportError>;
    
    /// Write bytes to the transport.
    async fn write(&self, data: &[u8]) -> Result<(), TransportError>;
    
    /// Close the connection.
    async fn close(&mut self) -> Result<(), TransportError>;
}
