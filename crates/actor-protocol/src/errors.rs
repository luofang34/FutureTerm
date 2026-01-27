//! Error Handling Guidelines
//!
//! All error messages should follow this format:
//!
//! 1. **What failed**: Describe the operation that failed
//! 2. **Why it failed**: Provide the root cause if known
//! 3. **What to do**: Suggest user action when possible
//!
//! Examples:
//! - ✅ "Failed to open serial port: Device already in use by another application. Close other programs and retry."
//! - ✅ "Port handle unavailable for probing - ensure Connect was called with valid port before probing."
//! - ❌ "No port handle available" (lacks context and action)
//! - ❌ "Error" (too vague)

use thiserror::Error;

/// Unified error type for actor operations
#[derive(Error, Debug, Clone)]
pub enum ActorError {
    /// State transition was rejected
    #[error("Invalid state transition: {0}")]
    InvalidTransition(String),

    /// Actor received an unexpected message in current state
    #[error("Unexpected message in state {state}: {message}")]
    UnexpectedMessage { state: String, message: String },

    /// Communication channel closed
    #[error("Channel closed: {0}")]
    ChannelClosed(String),

    /// Timeout waiting for response
    #[error("Operation timeout: {0}")]
    Timeout(String),

    /// Transport layer error
    #[error("Transport error: {0}")]
    Transport(String),

    /// Configuration error
    #[error("Configuration error: {0}")]
    Config(String),

    /// Device not found
    #[error("Device not found: {0}")]
    DeviceNotFound(String),

    /// Generic error
    #[error("{0}")]
    Other(String),
}

impl From<String> for ActorError {
    fn from(s: String) -> Self {
        ActorError::Other(s)
    }
}

impl From<&str> for ActorError {
    fn from(s: &str) -> Self {
        ActorError::Other(s.to_string())
    }
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn test_error_display() {
        let err = ActorError::InvalidTransition("Disconnected → Connected".into());
        assert_eq!(
            err.to_string(),
            "Invalid state transition: Disconnected → Connected"
        );
    }

    #[test]
    fn test_error_from_string() {
        let err: ActorError = "Test error".into();
        match err {
            ActorError::Other(msg) => assert_eq!(msg, "Test error"),
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_unexpected_message_error() {
        let err = ActorError::UnexpectedMessage {
            state: "Disconnected".into(),
            message: "ConnectionEstablished".into(),
        };
        assert!(err
            .to_string()
            .contains("Unexpected message in state Disconnected"));
    }
}
