use serde::{Deserialize, Serialize};

/// JSON-RPC style protocol for WebSocket communication
/// All messages follow the format: { "type": "...", "id": ..., "data": {...} }

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// List available serial ports
    ListPorts { id: u64 },

    /// Open a serial port with specified baud rate
    Open {
        id: u64,
        path: String,
        baud_rate: u32,
    },

    /// Close the currently open serial port
    Close { id: u64 },

    /// Write data to the serial port (base64 encoded)
    Write { id: u64, data: String },

    /// Set serial port configuration
    SetConfig {
        id: u64,
        baud_rate: Option<u32>,
        data_bits: Option<u8>,
        stop_bits: Option<u8>,
        parity: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Response to ListPorts
    PortsList { id: u64, ports: Vec<PortInfo> },

    /// Response to Open
    Opened { id: u64 },

    /// Response to Close
    Closed { id: u64 },

    /// Response to Write
    Written { id: u64, bytes: usize },

    /// Response to SetConfig
    ConfigSet { id: u64 },

    /// Serial data received (base64 encoded)
    Data { data: String },

    /// Error response
    Error { id: Option<u64>, message: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PortInfo {
    pub path: String,
    pub port_type: PortType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub vid: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub serial_number: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub manufacturer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub product: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PortType {
    UsbSerial,
    Bluetooth,
    Pci,
    Unknown,
}

impl ClientMessage {
    /// Parse a JSON string into a ClientMessage
    pub fn from_json(s: &str) -> Result<Self, String> {
        serde_json::from_str(s).map_err(|e| format!("Failed to parse message: {}", e))
    }

    /// Get the request ID if present
    pub fn id(&self) -> u64 {
        match self {
            ClientMessage::ListPorts { id }
            | ClientMessage::Open { id, .. }
            | ClientMessage::Close { id }
            | ClientMessage::Write { id, .. }
            | ClientMessage::SetConfig { id, .. } => *id,
        }
    }
}

impl ServerMessage {
    /// Serialize to JSON string
    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|e| format!("Failed to serialize message: {}", e))
    }

    /// Create an error message
    pub fn error(id: Option<u64>, message: impl Into<String>) -> Self {
        ServerMessage::Error {
            id,
            message: message.into(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_list_ports() {
        let json = r#"{"type":"list_ports","id":1}"#;
        let msg = ClientMessage::from_json(json).unwrap();
        match msg {
            ClientMessage::ListPorts { id } => assert_eq!(id, 1),
            _ => panic!("Wrong message type"),
        }
    }

    #[test]
    fn test_parse_open() {
        let json = r#"{"type":"open","id":2,"path":"/dev/cu.usbserial","baud_rate":115200}"#;
        let msg = ClientMessage::from_json(json).unwrap();
        match msg {
            ClientMessage::Open {
                id,
                path,
                baud_rate,
            } => {
                assert_eq!(id, 2);
                assert_eq!(path, "/dev/cu.usbserial");
                assert_eq!(baud_rate, 115200);
            }
            _ => panic!("Wrong message type"),
        }
    }

    #[test]
    fn test_parse_close() {
        let json = r#"{"type":"close","id":3}"#;
        let msg = ClientMessage::from_json(json).unwrap();
        match msg {
            ClientMessage::Close { id } => assert_eq!(id, 3),
            _ => panic!("Wrong message type"),
        }
    }

    #[test]
    fn test_parse_write() {
        let json = r#"{"type":"write","id":4,"data":"SGVsbG8="}"#;
        let msg = ClientMessage::from_json(json).unwrap();
        match msg {
            ClientMessage::Write { id, data } => {
                assert_eq!(id, 4);
                assert_eq!(data, "SGVsbG8=");
            }
            _ => panic!("Wrong message type"),
        }
    }

    #[test]
    fn test_parse_set_config() {
        let json = r#"{"type":"set_config","id":5,"baud_rate":57600,"data_bits":8}"#;
        let msg = ClientMessage::from_json(json).unwrap();
        match msg {
            ClientMessage::SetConfig {
                id,
                baud_rate,
                data_bits,
                stop_bits,
                parity,
            } => {
                assert_eq!(id, 5);
                assert_eq!(baud_rate, Some(57600));
                assert_eq!(data_bits, Some(8));
                assert_eq!(stop_bits, None);
                assert_eq!(parity, None);
            }
            _ => panic!("Wrong message type"),
        }
    }

    #[test]
    fn test_serialize_ports_list() {
        let msg = ServerMessage::PortsList {
            id: 1,
            ports: vec![PortInfo {
                path: "/dev/cu.usbserial".into(),
                port_type: PortType::UsbSerial,
                vid: Some(0x0403),
                pid: Some(0x6001),
                serial_number: Some("FTDI123".into()),
                manufacturer: Some("FTDI".into()),
                product: Some("USB Serial".into()),
            }],
        };
        let json = msg.to_json().unwrap();
        assert!(json.contains("ports_list"));
        assert!(json.contains("/dev/cu.usbserial"));
    }

    #[test]
    fn test_serialize_opened() {
        let msg = ServerMessage::Opened { id: 2 };
        let json = msg.to_json().unwrap();
        assert!(json.contains("opened"));
        assert!(json.contains("\"id\":2"));
    }

    #[test]
    fn test_serialize_data() {
        let msg = ServerMessage::Data {
            data: "SGVsbG8=".into(),
        };
        let json = msg.to_json().unwrap();
        assert!(json.contains("data"));
        assert!(json.contains("SGVsbG8="));
    }

    #[test]
    fn test_serialize_error() {
        let msg = ServerMessage::error(Some(3), "Port not found");
        let json = msg.to_json().unwrap();
        assert!(json.contains("error"));
        assert!(json.contains("Port not found"));
    }

    #[test]
    fn test_get_id() {
        let msg1 = ClientMessage::ListPorts { id: 42 };
        assert_eq!(msg1.id(), 42);

        let msg2 = ClientMessage::Close { id: 100 };
        assert_eq!(msg2.id(), 100);
    }

    #[test]
    fn test_invalid_json() {
        let result = ClientMessage::from_json("invalid json");
        assert!(result.is_err());
    }

    #[test]
    fn test_unknown_type() {
        let json = r#"{"type":"unknown","id":1}"#;
        let result = ClientMessage::from_json(json);
        assert!(result.is_err());
    }
}
