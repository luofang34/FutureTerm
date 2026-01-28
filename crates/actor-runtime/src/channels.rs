use actor_protocol::{SystemEvent, UiCommand};
use futures_channel::mpsc;

/// Message types for each actor in the system
/// Handle to a SerialPort object (WASM-only)
/// Clone is cheap (Rc increment)
#[cfg(target_arch = "wasm32")]
pub type PortHandle = std::rc::Rc<web_sys::SerialPort>;

#[derive(Clone)]
pub enum StateMessage {
    /// Commands from UI
    UiCommand(UiCommand),
    /// Command from UI with port handle (WASM-only)
    #[cfg(target_arch = "wasm32")]
    UiCommandWithPort {
        cmd: UiCommand,
        port_handle: PortHandle,
    },

    /// Internal messages from other actors
    ConnectionEstablished {
        /// Operation sequence number to match against expected operation
        operation_id: u32,
    },
    ConnectionFailed {
        reason: String,
    },
    ConnectionLost,
    /// Port has been fully closed (sent by PortActor after close completes)
    ConnectionClosed,
    ProbeComplete {
        baud: u32,
        framing: String,
        protocol: Option<String>,
        initial_data: Vec<u8>,
    },
    DeviceReappeared {
        port: actor_protocol::SerialPortInfo,
        /// Actual SerialPort object (WASM only)
        #[cfg(target_arch = "wasm32")]
        port_handle: PortHandle,
    },

    /// Operation timeout (supervision)
    /// Sent when an operation doesn't complete within expected time
    OperationTimeout {
        operation: String,
        state: actor_protocol::ConnectionState,
    },
}

// Manual Debug implementation to handle web_sys::SerialPort
impl std::fmt::Debug for StateMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UiCommand(cmd) => f.debug_tuple("UiCommand").field(cmd).finish(),
            #[cfg(target_arch = "wasm32")]
            Self::UiCommandWithPort { cmd, .. } => f
                .debug_struct("UiCommandWithPort")
                .field("cmd", cmd)
                .field("port_handle", &"<SerialPort>")
                .finish(),
            Self::ConnectionEstablished { operation_id } => f
                .debug_struct("ConnectionEstablished")
                .field("operation_id", operation_id)
                .finish(),
            Self::ConnectionFailed { reason } => f
                .debug_struct("ConnectionFailed")
                .field("reason", reason)
                .finish(),
            Self::ConnectionLost => write!(f, "ConnectionLost"),
            Self::ConnectionClosed => write!(f, "ConnectionClosed"),
            Self::ProbeComplete {
                baud,
                framing,
                protocol,
                initial_data,
            } => f
                .debug_struct("ProbeComplete")
                .field("baud", baud)
                .field("framing", framing)
                .field("protocol", protocol)
                .field("initial_data", initial_data)
                .finish(),
            #[cfg(target_arch = "wasm32")]
            Self::DeviceReappeared { port, .. } => f
                .debug_struct("DeviceReappeared")
                .field("port", port)
                .field("port_handle", &"<SerialPort>")
                .finish(),
            #[cfg(not(target_arch = "wasm32"))]
            Self::DeviceReappeared { port } => f
                .debug_struct("DeviceReappeared")
                .field("port", port)
                .finish(),
            Self::OperationTimeout { operation, state } => f
                .debug_struct("OperationTimeout")
                .field("operation", operation)
                .field("state", state)
                .finish(),
        }
    }
}

#[derive(Clone)]
pub enum PortMessage {
    Open {
        port: actor_protocol::SerialPortInfo,
        baud: u32,
        framing: String,
        send_wakeup: bool, // True if from auto-detection, false otherwise
        /// Operation sequence number to track this specific operation
        operation_id: u32,
        /// Actual SerialPort object (WASM only)
        #[cfg(target_arch = "wasm32")]
        port_handle: PortHandle,
    },
    Close,
    Write {
        data: Vec<u8>,
    },
    InjectData {
        data: Vec<u8>,
    },
}

// Manual Debug implementation
impl std::fmt::Debug for PortMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(target_arch = "wasm32")]
            Self::Open {
                port,
                baud,
                framing,
                send_wakeup,
                operation_id,
                ..
            } => f
                .debug_struct("Open")
                .field("port", port)
                .field("baud", baud)
                .field("framing", framing)
                .field("send_wakeup", send_wakeup)
                .field("operation_id", operation_id)
                .field("port_handle", &"<SerialPort>")
                .finish(),
            #[cfg(not(target_arch = "wasm32"))]
            Self::Open {
                port,
                baud,
                framing,
                send_wakeup,
                operation_id,
            } => f
                .debug_struct("Open")
                .field("port", port)
                .field("baud", baud)
                .field("framing", framing)
                .field("send_wakeup", send_wakeup)
                .field("operation_id", operation_id)
                .finish(),
            Self::Close => write!(f, "Close"),
            Self::Write { data } => f.debug_struct("Write").field("data", data).finish(),
            Self::InjectData { data } => f.debug_struct("InjectData").field("data", data).finish(),
        }
    }
}

#[derive(Clone)]
pub enum ProbeMessage {
    Start {
        port: actor_protocol::SerialPortInfo,
        /// Actual SerialPort object (WASM only)
        #[cfg(target_arch = "wasm32")]
        port_handle: PortHandle,
    },
    Abort,
}

// Manual Debug implementation
impl std::fmt::Debug for ProbeMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            #[cfg(target_arch = "wasm32")]
            Self::Start { port, .. } => f
                .debug_struct("Start")
                .field("port", port)
                .field("port_handle", &"<SerialPort>")
                .finish(),
            #[cfg(not(target_arch = "wasm32"))]
            Self::Start { port } => f.debug_struct("Start").field("port", port).finish(),
            Self::Abort => write!(f, "Abort"),
        }
    }
}

#[derive(Clone)]
pub enum ReconnectMessage {
    RegisterDevice {
        vid: u16,
        pid: u16,
        config: actor_protocol::SerialConfig,
    },
    ClearDevice,
    DeviceConnected {
        port: actor_protocol::SerialPortInfo,
        /// Actual SerialPort object (WASM only)
        #[cfg(target_arch = "wasm32")]
        port_handle: PortHandle,
    },
}

// Manual Debug implementation
impl std::fmt::Debug for ReconnectMessage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RegisterDevice { vid, pid, config } => f
                .debug_struct("RegisterDevice")
                .field("vid", vid)
                .field("pid", pid)
                .field("config", config)
                .finish(),
            Self::ClearDevice => write!(f, "ClearDevice"),
            #[cfg(target_arch = "wasm32")]
            Self::DeviceConnected { port, .. } => f
                .debug_struct("DeviceConnected")
                .field("port", port)
                .field("port_handle", &"<SerialPort>")
                .finish(),
            #[cfg(not(target_arch = "wasm32"))]
            Self::DeviceConnected { port } => f
                .debug_struct("DeviceConnected")
                .field("port", port)
                .finish(),
        }
    }
}

/// Handles for spawning actors
pub struct ActorHandles {
    pub state_rx: mpsc::Receiver<StateMessage>,
    pub port_rx: mpsc::Receiver<PortMessage>,
    pub probe_rx: mpsc::Receiver<ProbeMessage>,
    pub reconnect_rx: mpsc::Receiver<ReconnectMessage>,
    pub event_tx: mpsc::Sender<SystemEvent>,
}

/// Channel manager for actor communication
///
/// This manages all communication channels between actors and provides
/// a unified interface for sending messages.
pub struct ChannelManager {
    // Senders for each actor (all Clone)
    // Using bounded channels to prevent memory exhaustion under high load
    state_tx: mpsc::Sender<StateMessage>,
    port_tx: mpsc::Sender<PortMessage>,
    probe_tx: mpsc::Sender<ProbeMessage>,
    reconnect_tx: mpsc::Sender<ReconnectMessage>,

    // Event receiver (NOT cloned, replaced with dummy in Clone impl)
    // Note: Clone creates a disconnected receiver - use take_event_receiver() before cloning
    event_rx: mpsc::Receiver<SystemEvent>,
}

impl Clone for ChannelManager {
    fn clone(&self) -> Self {
        // Create a dummy receiver with same capacity as original for API compatibility
        // Note: This receiver is intentionally disconnected and will never receive messages
        // The real event_rx should be taken with take_event_receiver() before cloning
        let (_dummy_tx, dummy_rx) = mpsc::channel(1);
        Self {
            state_tx: self.state_tx.clone(),
            port_tx: self.port_tx.clone(),
            probe_tx: self.probe_tx.clone(),
            reconnect_tx: self.reconnect_tx.clone(),
            event_rx: dummy_rx, // Dummy receiver (disconnected)
        }
    }
}

impl ChannelManager {
    /// Create a new channel manager and actor handles
    ///
    /// Returns (ChannelManager for UI, ActorHandles for spawning actors)
    ///
    /// Channel capacities are sized for high-speed serial ports (5M-10M baud):
    /// - state_tx: 256 - State coordination messages (low frequency)
    /// - port_tx: 512 - Port I/O control messages (moderate frequency)
    /// - probe_tx: 128 - Probe control messages (low frequency)
    /// - reconnect_tx: 128 - USB reconnection messages (low frequency)
    /// - event_tx: 8192 - Data events for UI (high frequency, supports 10M baud @ 200ms UI latency)
    pub fn new() -> (Self, ActorHandles) {
        let (state_tx, state_rx) = mpsc::channel(256);
        let (port_tx, port_rx) = mpsc::channel(512);
        let (probe_tx, probe_rx) = mpsc::channel(128);
        let (reconnect_tx, reconnect_rx) = mpsc::channel(128);
        let (event_tx, event_rx) = mpsc::channel(8192);

        let handles = ActorHandles {
            state_rx,
            port_rx,
            probe_rx,
            reconnect_rx,
            event_tx,
        };

        let manager = Self {
            state_tx,
            port_tx,
            probe_tx,
            reconnect_tx,
            event_rx,
        };

        (manager, handles)
    }

    /// Send a UI command to the appropriate actor
    pub fn send_command(&self, cmd: UiCommand) -> Result<(), String> {
        match cmd {
            UiCommand::Connect { .. }
            | UiCommand::Disconnect
            | UiCommand::SetDecoder { .. }
            | UiCommand::SetFramer { .. }
            | UiCommand::Reconfigure { .. } => {
                self.state_tx
                    .clone()
                    .try_send(StateMessage::UiCommand(cmd))
                    .map_err(|e| {
                        #[cfg(all(debug_assertions, target_arch = "wasm32"))]
                        web_sys::console::error_1(
                            &format!("CRITICAL: StateActor channel error: {:?}", e).into(),
                        );

                        if e.is_full() {
                            "System overloaded: Too many pending commands. Please slow down."
                                .to_string()
                        } else {
                            "System error: State management unavailable. Please refresh the page."
                                .to_string()
                        }
                    })?;
            }
            UiCommand::WriteData { data } => {
                self.port_tx
                    .clone()
                    .try_send(PortMessage::Write { data })
                    .map_err(|e| {
                        #[cfg(all(debug_assertions, target_arch = "wasm32"))]
                        web_sys::console::error_1(
                            &format!("CRITICAL: PortActor channel error: {:?}", e).into(),
                        );

                        if e.is_full() {
                            "System overloaded: Write queue full. Data transmission too fast."
                                .to_string()
                        } else {
                            "System error: Port communication unavailable. Please refresh the page."
                                .to_string()
                        }
                    })?;
            }
        }
        Ok(())
    }

    /// Send a UI command with port handle (WASM-only)
    ///
    /// This avoids global mutable state by passing the SerialPort through messages
    #[cfg(target_arch = "wasm32")]
    pub fn send_command_with_port(
        &self,
        cmd: UiCommand,
        port: web_sys::SerialPort,
    ) -> Result<(), String> {
        let port_handle = std::rc::Rc::new(port);
        self.state_tx
            .clone()
            .try_send(StateMessage::UiCommandWithPort { cmd, port_handle })
            .map_err(|e| {
                if e.is_full() {
                    "System overloaded: Command queue full.".to_string()
                } else {
                    "StateActor channel closed".to_string()
                }
            })?;
        Ok(())
    }

    /// Get mutable reference to event receiver
    ///
    /// This allows the UI to poll for events from actors
    pub fn event_receiver(&mut self) -> &mut mpsc::Receiver<SystemEvent> {
        &mut self.event_rx
    }

    /// Take ownership of event receiver
    ///
    /// This allows the UI to move the receiver into a spawn_local task
    pub fn take_event_receiver(&mut self) -> mpsc::Receiver<SystemEvent> {
        let (_new_tx, new_rx) = mpsc::channel(1);
        // Note: _new_tx is dropped, so events sent after this call will be lost
        // This is intentional - the receiver should only be taken once
        std::mem::replace(&mut self.event_rx, new_rx)
    }

    /// Clone senders for direct actor-to-actor communication
    ///
    /// These clones can be passed to actors for internal messaging
    pub fn state_sender(&self) -> mpsc::Sender<StateMessage> {
        self.state_tx.clone()
    }

    pub fn port_sender(&self) -> mpsc::Sender<PortMessage> {
        self.port_tx.clone()
    }

    pub fn probe_sender(&self) -> mpsc::Sender<ProbeMessage> {
        self.probe_tx.clone()
    }

    pub fn reconnect_sender(&self) -> mpsc::Sender<ReconnectMessage> {
        self.reconnect_tx.clone()
    }
}

impl Default for ChannelManager {
    fn default() -> Self {
        Self::new().0
    }
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use futures::stream::StreamExt;

    #[tokio::test]
    async fn test_channel_manager_creation() {
        let (_manager, _handles) = ChannelManager::new();
        // Just verify it can be created
    }

    #[tokio::test]
    async fn test_send_connect_command() {
        let (manager, mut handles) = ChannelManager::new();

        let cmd = UiCommand::Connect {
            port: actor_protocol::SerialPortInfo::new("/dev/ttyUSB0".into(), None, None),
            baud: 115200,
            framing: "8N1".to_string(),
        };

        manager.send_command(cmd).unwrap();

        // Verify message was routed to StateActor
        let msg = handles.state_rx.next().await.unwrap();
        match msg {
            StateMessage::UiCommand(UiCommand::Connect { baud, framing, .. }) => {
                assert_eq!(baud, 115200);
                assert_eq!(framing, "8N1");
            }
            _ => panic!("Wrong message type"),
        }
    }

    #[tokio::test]
    async fn test_send_write_command() {
        let (manager, mut handles) = ChannelManager::new();

        let cmd = UiCommand::WriteData {
            data: vec![1, 2, 3],
        };

        manager.send_command(cmd).unwrap();

        // Verify message was routed to PortActor
        let msg = handles.port_rx.next().await.unwrap();
        match msg {
            PortMessage::Write { data } => {
                assert_eq!(data, vec![1, 2, 3]);
            }
            _ => panic!("Wrong message type"),
        }
    }

    #[tokio::test]
    async fn test_event_receiver() {
        let (mut manager, mut handles) = ChannelManager::new();

        // Simulate an actor sending an event
        handles
            .event_tx
            .try_send(SystemEvent::StatusUpdate {
                message: "Test".into(),
            })
            .ok();

        // Drop handles to close channels
        drop(handles);

        // Receive event
        let event = manager.event_receiver().next().await.unwrap();
        match event {
            SystemEvent::StatusUpdate { message } => {
                assert_eq!(message, "Test");
            }
            _ => panic!("Wrong event type"),
        }
    }

    #[tokio::test]
    async fn test_actor_to_actor_messaging() {
        let (manager, mut handles) = ChannelManager::new();

        // Get a clone of the state sender (as another actor would)
        let mut state_tx = manager.state_sender();

        // Simulate ProbeActor sending ProbeComplete to StateActor
        state_tx
            .try_send(StateMessage::ProbeComplete {
                baud: 115200,
                framing: "8N1".into(),
                protocol: Some("mavlink".into()),
                initial_data: vec![0, 1, 2],
            })
            .ok();

        // Verify StateActor receives it
        let msg = handles.state_rx.next().await.unwrap();
        match msg {
            StateMessage::ProbeComplete {
                baud,
                framing,
                protocol,
                initial_data,
            } => {
                assert_eq!(baud, 115200);
                assert_eq!(framing, "8N1");
                assert_eq!(protocol, Some("mavlink".into()));
                assert_eq!(initial_data, vec![0, 1, 2]);
            }
            _ => panic!("Wrong message type"),
        }
    }
}
