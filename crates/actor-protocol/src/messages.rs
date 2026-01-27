use crate::state::ConnectionState;
use serde::{Deserialize, Serialize};

/// Serial port information for connection requests
/// This is a simplified representation that can be serialized
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SerialPortInfo {
    pub path: String,
    pub vid: Option<u16>,
    pub pid: Option<u16>,
}

impl SerialPortInfo {
    pub fn new(path: String, vid: Option<u16>, pid: Option<u16>) -> Self {
        Self { path, vid, pid }
    }
}

/// Serial configuration parameters
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SerialConfig {
    pub baud_rate: u32,
    pub data_bits: u8,
    pub stop_bits: u8,
    pub parity: ParityMode,
    pub flow_control: FlowControl,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum ParityMode {
    None,
    Even,
    Odd,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
pub enum FlowControl {
    None,
    Hardware,
    Software,
}

impl SerialConfig {
    /// Create a standard 8N1 configuration at specified baud rate
    pub fn new_8n1(baud_rate: u32) -> Self {
        Self {
            baud_rate,
            data_bits: 8,
            stop_bits: 1,
            parity: ParityMode::None,
            flow_control: FlowControl::None,
        }
    }

    /// Parse framing string (e.g., "8N1") into configuration
    pub fn from_framing(_framing: &str, baud_rate: u32) -> Self {
        // For now, default to 8N1 - can be extended to parse more formats
        Self::new_8n1(baud_rate)
    }
}

/// Commands from UI to Actor system
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum UiCommand {
    /// Request to connect to a serial port
    Connect {
        port: SerialPortInfo,
        /// Baud rate (0 = auto-detect)
        baud: u32,
        /// Framing configuration (e.g., "8N1", "8E1", "7E1", "Auto")
        framing: String,
    },

    /// Request to disconnect from current port
    Disconnect,

    /// Write data to connected port
    WriteData { data: Vec<u8> },

    /// Change the active decoder (for protocol parsing)
    SetDecoder { id: String },

    /// Change the active framer (for frame detection)
    SetFramer { id: String },

    /// Reconfigure connection parameters
    Reconfigure { baud: u32, framing: String },
}

/// Events from Actor system to UI
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SystemEvent {
    /// Connection state has changed
    StateChanged { state: ConnectionState },

    /// Status message for user display
    StatusUpdate { message: String },

    /// Data received from serial port
    DataReceived { data: Vec<u8>, timestamp_us: u64 },

    /// Progress update during auto-detection
    ProbeProgress { baud: u32, message: String },

    /// Error occurred
    Error { message: String },

    /// RX activity indicator (for UI blinking)
    RxActivity,

    /// TX activity indicator (for UI blinking)
    TxActivity,

    /// Decoder should be changed (e.g., when MAVLink detected)
    DecoderChanged { id: String },
}

/// Internal messages between actors (not serialized, only used in-process)
///
/// **NOTE**: This enum is not actively used in the current implementation.
/// The system uses actor-specific message types (StateMessage, PortMessage, etc.)
/// defined in actor-runtime instead. This is kept for API compatibility.
#[derive(Debug, Clone)]
pub enum InternalMessage {
    // StateActor → PortActor
    StartConnection {
        port: SerialPortInfo,
        config: SerialConfig,
    },
    StopConnection,

    // PortActor → StateActor
    ConnectionEstablished,
    ConnectionFailed {
        reason: String,
    },
    ConnectionLost,

    // ProbeActor → StateActor
    ProbeComplete {
        baud: u32,
        framing: String,
        protocol: Option<String>,
        initial_data: Vec<u8>,
    },
    ProbeFailed {
        reason: String,
    },

    // ReconnectActor → StateActor
    DeviceReappeared {
        port: SerialPortInfo,
    },

    // StateActor → ProbeActor
    StartProbe {
        port: SerialPortInfo,
    },
    AbortProbe,

    // StateActor → ReconnectActor
    RegisterDevice {
        vid: u16,
        pid: u16,
        config: SerialConfig,
    },
    ClearDevice,
}

/// Result of auto-detection probing
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ProbeResult {
    pub baud: u32,
    pub framing: String,
    pub protocol: Option<String>,
    pub initial_data: Vec<u8>,
    pub confidence: f64,
}

impl Default for ProbeResult {
    fn default() -> Self {
        Self {
            baud: 115200,
            framing: "8N1".into(),
            protocol: None,
            initial_data: Vec::new(),
            confidence: 0.0,
        }
    }
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn test_serial_port_info_serialization() {
        let info = SerialPortInfo::new("/dev/ttyUSB0".into(), Some(0x1234), Some(0x5678));
        let json = serde_json::to_string(&info).unwrap();
        let deserialized: SerialPortInfo = serde_json::from_str(&json).unwrap();
        assert_eq!(info, deserialized);
    }

    #[test]
    fn test_ui_command_serialization() {
        let cmd = UiCommand::Connect {
            port: SerialPortInfo::new("/dev/ttyUSB0".into(), None, None),
            baud: 115200,
            framing: "8N1".to_string(),
        };
        let json = serde_json::to_string(&cmd).unwrap();
        let deserialized: UiCommand = serde_json::from_str(&json).unwrap();

        match deserialized {
            UiCommand::Connect {
                port,
                baud,
                framing,
            } => {
                assert_eq!(port.path, "/dev/ttyUSB0");
                assert_eq!(baud, 115200);
                assert_eq!(framing, "8N1");
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_system_event_serialization() {
        let event = SystemEvent::StateChanged {
            state: ConnectionState::Connected,
        };
        let json = serde_json::to_string(&event).unwrap();
        let deserialized: SystemEvent = serde_json::from_str(&json).unwrap();

        match deserialized {
            SystemEvent::StateChanged { state } => {
                assert_eq!(state, ConnectionState::Connected);
            }
            _ => panic!("Wrong variant"),
        }
    }

    #[test]
    fn test_serial_config_8n1() {
        let config = SerialConfig::new_8n1(115200);
        assert_eq!(config.baud_rate, 115200);
        assert_eq!(config.data_bits, 8);
        assert_eq!(config.stop_bits, 1);
        assert_eq!(config.parity, ParityMode::None);
    }

    #[test]
    fn test_probe_result_default() {
        let result = ProbeResult::default();
        assert_eq!(result.baud, 115200);
        assert_eq!(result.framing, "8N1");
        assert_eq!(result.protocol, None);
        assert_eq!(result.confidence, 0.0);
    }
}
