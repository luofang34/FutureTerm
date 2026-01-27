use actor_protocol::{ActorError, ConnectionState, SystemEvent, UiCommand};
use actor_runtime::{
    actor_debug, actor_info, spawn_timeout, Actor, PortMessage, ProbeMessage, ReconnectMessage,
    StateMessage, SupervisionConfig, TimeoutHandle,
};
use futures_channel::mpsc;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;

use crate::data_processing::{detect_meaningful_content, trim_shell_artifacts};

/// StateActor manages the connection state machine and coordinates other actors
///
/// Responsibilities:
/// - Maintain single source of truth for connection state
/// - Validate and execute state transitions
/// - Route commands to appropriate actors (PortActor, ProbeActor, ReconnectActor)
/// - Emit state change events to UI
///
/// ## State Machine
///
/// For a complete state transition diagram and invariants, see:
/// `actor-protocol/src/state.rs` - ConnectionState documentation
///
/// Key coordination patterns:
/// - **Event-driven disconnect**: Disconnecting → ConnectionClosed → Disconnected
/// - **Event-driven reconfigure**: Reconfiguring → Disconnecting → ConnectionClosed → Connecting
/// - **USB hotplug**: Connected → DeviceLost → AutoReconnecting → Connected
pub struct StateActor {
    state: ConnectionState,
    port_tx: mpsc::Sender<PortMessage>,
    probe_tx: mpsc::Sender<ProbeMessage>,
    reconnect_tx: mpsc::Sender<ReconnectMessage>,
    event_tx: mpsc::Sender<SystemEvent>,

    // Channel to send messages to self (for timeouts)
    state_tx: mpsc::Sender<StateMessage>,

    // Supervision configuration
    supervision_config: SupervisionConfig,

    // Active timeout handle - automatically cancelled when StateActor transitions state
    active_timeout: Option<TimeoutHandle>,

    // Store port info and config for reconnection
    pending_port: Option<actor_protocol::SerialPortInfo>,
    pending_baud: u32,

    // Operation sequence tracking for detecting stale responses
    // Incremented on each Open operation, used to validate ConnectionEstablished
    operation_sequence: u32,

    // Store port handle (WASM-only) - replaces PENDING_PORT global
    #[cfg(target_arch = "wasm32")]
    pending_port_handle: Option<actor_runtime::channels::PortHandle>,

    // Store reconfigure parameters for event-driven reconnection
    #[cfg(target_arch = "wasm32")]
    pending_reconfigure_baud: Option<u32>,
    #[cfg(target_arch = "wasm32")]
    pending_reconfigure_framing: Option<String>,
}

impl StateActor {
    pub fn new(
        port_tx: mpsc::Sender<PortMessage>,
        probe_tx: mpsc::Sender<ProbeMessage>,
        reconnect_tx: mpsc::Sender<ReconnectMessage>,
        event_tx: mpsc::Sender<SystemEvent>,
        state_tx: mpsc::Sender<StateMessage>,
    ) -> Self {
        Self {
            state: ConnectionState::Disconnected,
            port_tx,
            probe_tx,
            reconnect_tx,
            event_tx,
            state_tx,
            supervision_config: SupervisionConfig::default(),
            active_timeout: None,
            pending_port: None,
            pending_baud: 0,
            operation_sequence: 0,
            #[cfg(target_arch = "wasm32")]
            pending_port_handle: None,
            #[cfg(target_arch = "wasm32")]
            pending_reconfigure_baud: None,
            #[cfg(target_arch = "wasm32")]
            pending_reconfigure_framing: None,
        }
    }

    /// Get next operation ID for tracking port open operations
    ///
    /// Increments internal sequence counter and returns new value.
    /// Used to detect stale ConnectionEstablished messages after timeout.
    fn next_operation_id(&mut self) -> u32 {
        self.operation_sequence = self.operation_sequence.wrapping_add(1);
        self.operation_sequence
    }

    /// Send a CRITICAL message that must succeed for system correctness
    ///
    /// If the channel is closed, the target actor has crashed or shut down.
    /// If the channel is full, the system is overloaded.
    /// Both cases are fatal and should propagate as errors.
    fn send_critical_port(&self, msg: PortMessage) -> Result<(), ActorError> {
        self.port_tx.clone().try_send(msg).map_err(|e| {
            if e.is_disconnected() {
                ActorError::ChannelClosed("PortActor has shut down".into())
            } else {
                ActorError::Other("PortActor channel overloaded".into())
            }
        })
    }

    fn send_critical_probe(&self, msg: ProbeMessage) -> Result<(), ActorError> {
        self.probe_tx.clone().try_send(msg).map_err(|e| {
            if e.is_disconnected() {
                ActorError::ChannelClosed("ProbeActor has shut down".into())
            } else {
                ActorError::Other("ProbeActor channel overloaded".into())
            }
        })
    }

    /// Send a WARNING-level message (UI events, device registration)
    ///
    /// Failures are logged but don't propagate - these are non-critical for core FSM logic
    fn send_ui_event(&self, event: SystemEvent) {
        if let Err(e) = self.event_tx.clone().try_send(event) {
            #[cfg(debug_assertions)]
            {
                #[cfg(target_arch = "wasm32")]
                web_sys::console::warn_1(&format!("UI event dropped: {:?}", e).into());
                #[cfg(not(target_arch = "wasm32"))]
                eprintln!("WARNING: UI event dropped: {:?}", e);
            }
        }
    }

    fn send_reconnect_hint(&self, msg: ReconnectMessage) {
        if let Err(e) = self.reconnect_tx.clone().try_send(msg) {
            #[cfg(debug_assertions)]
            {
                #[cfg(target_arch = "wasm32")]
                web_sys::console::warn_1(&format!("Reconnect hint dropped: {:?}", e).into());
                #[cfg(not(target_arch = "wasm32"))]
                eprintln!("WARNING: Reconnect hint dropped: {:?}", e);
            }
        }
    }

    /// Attempt to transition to a new state
    ///
    /// Returns Ok if transition is valid, Err otherwise
    fn transition(&mut self, new_state: ConnectionState) -> Result<(), ActorError> {
        if !self.state.can_transition_to(new_state) {
            return Err(ActorError::InvalidTransition(format!(
                "{:?} → {:?}",
                self.state, new_state
            )));
        }

        #[cfg(debug_assertions)]
        let old_state = self.state;

        // Cancel any active timeout from previous state
        if let Some(handle) = self.active_timeout.take() {
            handle.cancel();
            actor_debug!("Cancelled timeout for previous state");
        }

        self.state = new_state;

        // Notify UI of state change (non-critical)
        self.send_ui_event(SystemEvent::StateChanged { state: new_state });

        actor_debug!("State: {:?} → {:?}", old_state, new_state);

        // Spawn supervision timeout for long-running states
        self.active_timeout = self.spawn_supervision_timeout_if_needed(new_state);

        Ok(())
    }

    /// Spawn a supervision timeout for states that might hang
    ///
    /// Returns a TimeoutHandle that will be stored in `active_timeout` and automatically
    /// cancelled when the state transitions. This prevents spurious timeout messages.
    fn spawn_supervision_timeout_if_needed(&self, state: ConnectionState) -> Option<TimeoutHandle> {
        let (operation, timeout_secs) = match state {
            ConnectionState::Probing => ("Probing", self.supervision_config.probe_timeout_secs),
            ConnectionState::Connecting => {
                ("Connecting", self.supervision_config.connect_timeout_secs)
            }
            ConnectionState::AutoReconnecting => (
                "AutoReconnecting",
                self.supervision_config.auto_reconnect_timeout_secs,
            ),
            ConnectionState::Disconnecting => (
                "Disconnecting",
                self.supervision_config.disconnect_timeout_secs,
            ),
            ConnectionState::Reconfiguring => (
                "Reconfiguring",
                self.supervision_config.reconfigure_timeout_secs,
            ),
            // Connected, Disconnected, DeviceLost don't need timeouts (stable states or waiting for external event)
            _ => return None,
        };

        actor_debug!("Spawning {} second timeout for {}", timeout_secs, operation);
        let handle = spawn_timeout(self.state_tx.clone(), operation, state, timeout_secs);
        Some(handle)
    }

    async fn handle_connect(
        &mut self,
        port: actor_protocol::SerialPortInfo,
        baud: u32,
        framing: String,
        #[cfg(target_arch = "wasm32")] port_handle: Option<actor_runtime::channels::PortHandle>,
    ) -> Result<(), ActorError> {
        // Validate current state
        if self.state != ConnectionState::Disconnected {
            return Err(ActorError::UnexpectedMessage {
                state: format!("{:?}", self.state),
                message: "Connect".into(),
            });
        }

        // Normalize framing - if "Auto" or empty, default to "8N1" for non-probing connections
        let actual_framing = if framing.is_empty() || framing.eq_ignore_ascii_case("auto") {
            "8N1".to_string()
        } else {
            framing
        };

        // Store port handle for later use
        #[cfg(target_arch = "wasm32")]
        {
            self.pending_port_handle = port_handle.clone();
        }

        if baud == 0 {
            // Auto-detect requested - store port and go to Probing
            self.pending_port = Some(port.clone());
            self.pending_baud = 0; // Will be set by ProbeComplete
            self.transition(ConnectionState::Probing)?;

            #[cfg(target_arch = "wasm32")]
            {
                if let Some(handle) = port_handle {
                    self.send_critical_probe(ProbeMessage::Start {
                        port,
                        port_handle: handle,
                    })?;
                } else {
                    return Err(ActorError::Transport("No port handle provided".into()));
                }
            }

            #[cfg(not(target_arch = "wasm32"))]
            {
                self.send_critical_probe(ProbeMessage::Start { port })?;
            }
        } else {
            // Direct connection with specified baud (no auto-detection)
            self.pending_port = Some(port.clone());
            self.pending_baud = baud;
            self.transition(ConnectionState::Connecting)?;

            let operation_id = self.next_operation_id();

            #[cfg(target_arch = "wasm32")]
            {
                if let Some(handle) = port_handle {
                    self.send_critical_port(PortMessage::Open {
                        port,
                        baud,
                        framing: actual_framing,
                        send_wakeup: true,
                        operation_id,
                        port_handle: handle,
                    })?;
                } else {
                    return Err(ActorError::Transport("No port handle provided".into()));
                }
            }

            #[cfg(not(target_arch = "wasm32"))]
            {
                self.send_critical_port(PortMessage::Open {
                    port,
                    baud,
                    framing: actual_framing,
                    send_wakeup: true,
                    operation_id,
                })?;
            }
        }

        Ok(())
    }

    async fn handle_disconnect(&mut self) -> Result<(), ActorError> {
        // Can disconnect from most states
        if !self.state.can_disconnect() {
            return Err(ActorError::UnexpectedMessage {
                state: format!("{:?}", self.state),
                message: "Disconnect".into(),
            });
        }

        // Abort any ongoing probe (CRITICAL - must succeed to cancel operation)
        if self.state == ConnectionState::Probing {
            self.send_critical_probe(ProbeMessage::Abort)?;
        }

        // Clear pending port
        self.pending_port = None;
        self.pending_baud = 0;

        // Clear device registration for auto-reconnect (non-critical hint)
        self.send_reconnect_hint(ReconnectMessage::ClearDevice);

        self.transition(ConnectionState::Disconnecting)?;

        // Tell PortActor to close (CRITICAL - must succeed for resource cleanup)
        self.send_critical_port(PortMessage::Close)?;

        // Event-driven coordination: StateActor will receive ConnectionClosed
        // message from PortActor when close is complete, then transition to Disconnected
        // (See handle() method for ConnectionClosed case)

        Ok(())
    }

    async fn handle_connection_established(&mut self, operation_id: u32) -> Result<(), ActorError> {
        // Can happen from Connecting (manual connect) or AutoReconnecting (USB replug)
        if self.state != ConnectionState::Connecting
            && self.state != ConnectionState::AutoReconnecting
        {
            return Err(ActorError::UnexpectedMessage {
                state: format!("{:?}", self.state),
                message: "ConnectionEstablished".into(),
            });
        }

        // Validate operation ID to prevent orphan ports from timed-out operations
        if operation_id != self.operation_sequence {
            actor_debug!(
                "Ignoring stale ConnectionEstablished (operation_id={}, expected={})",
                operation_id,
                self.operation_sequence
            );

            // Close the orphan port to prevent resource leak (CRITICAL)
            self.send_critical_port(PortMessage::Close)?;

            return Err(ActorError::InvalidTransition(
                "Stale ConnectionEstablished".to_string(),
            ));
        }

        self.transition(ConnectionState::Connected)?;

        // Register device for auto-reconnect if we have VID/PID (non-critical hint)
        if let Some(ref port) = self.pending_port {
            if let (Some(vid), Some(pid)) = (port.vid, port.pid) {
                let config = actor_protocol::SerialConfig::new_8n1(self.pending_baud);

                self.send_reconnect_hint(ReconnectMessage::RegisterDevice { vid, pid, config });

                #[cfg(debug_assertions)]
                {
                    actor_info!(
                        "Registered device {:04X}:{:04X} for auto-reconnect",
                        vid,
                        pid
                    );
                }
            }
        }

        Ok(())
    }

    async fn handle_connection_failed(&mut self, reason: String) -> Result<(), ActorError> {
        // Emit error event (non-critical UI notification)
        self.send_ui_event(SystemEvent::Error {
            message: format!("Connection failed: {}", reason),
        });

        // Return to disconnected
        self.transition(ConnectionState::Disconnected)?;

        Ok(())
    }

    async fn handle_probe_complete(
        &mut self,
        baud: u32,
        framing: String,
        protocol: Option<String>,
        initial_data: Vec<u8>,
    ) -> Result<(), ActorError> {
        if self.state != ConnectionState::Probing {
            // Fix #9: Race condition - User disconnected during probe
            // Just ignore the message instead of erroring
            actor_debug!(
                "StateActor: Ignoring ProbeComplete in {:?} state",
                self.state
            );
            return Ok(());
        }

        // Notify UI of detection result (non-critical)
        let msg = if let Some(ref proto) = protocol {
            format!("Detected: {} @ {} baud ({})", proto, baud, framing)
        } else {
            format!("Detected: {} baud ({})", baud, framing)
        };

        self.send_ui_event(SystemEvent::StatusUpdate { message: msg });

        // If protocol detected (e.g., MAVLink), change decoder automatically
        if let Some(ref proto) = protocol {
            self.send_ui_event(SystemEvent::DecoderChanged { id: proto.clone() });
        }

        // Store detected baud rate for later device registration
        self.pending_baud = baud;

        // Transition to connecting
        self.transition(ConnectionState::Connecting)?;

        // CRITICAL: Open the port with detected settings
        // Clone pending_port instead of taking it, so it's still available for device registration
        if let Some(port) = self.pending_port.clone() {
            // Start with detected settings
            // Protocol-Aware Logic:
            // - If a protocol (MAVLink, etc) is detected, preserve data exactly (vital stream).
            // - If Unknown/Raw (likely Shell), we want the prompt on Line 1.
            //   The probe sent '\r', which likely echoed '\r\n' before the prompt.
            //   We then DISABLE wakeup to avoid a second prompt.
            let (send_wakeup, data_to_inject) = if protocol.is_some() {
                // Protocol detected (e.g., MAVLink) - preserve data exactly
                (false, initial_data)
            } else {
                // Unknown/Raw data (likely shell) - clean up ANSI and whitespace
                let trimmed = trim_shell_artifacts(&initial_data);

                actor_debug!(
                    "StateActor: Smart Trimmed! Skipped {} bytes. New Len: {}",
                    initial_data.len() - trimmed.len(),
                    trimmed.len()
                );

                // Check if trimmed data contains meaningful content
                let is_meaningful = detect_meaningful_content(&trimmed);

                if trimmed.is_empty() || !is_meaningful {
                    actor_debug!("StateActor: Data not meaningful. Forcing Wakeup.");
                    (true, Vec::new())
                } else {
                    actor_debug!("StateActor: Content detected! Injecting.");
                    (false, trimmed)
                }
            };

            let operation_id = self.next_operation_id();

            #[cfg(target_arch = "wasm32")]
            {
                if let Some(handle) = self.pending_port_handle.clone() {
                    self.send_critical_port(PortMessage::Open {
                        port,
                        baud,
                        framing,
                        send_wakeup,
                        operation_id,
                        port_handle: handle,
                    })?;
                }
            }

            #[cfg(not(target_arch = "wasm32"))]
            {
                self.send_critical_port(PortMessage::Open {
                    port,
                    baud,
                    framing,
                    send_wakeup,
                    operation_id,
                })?;
            }

            // Inject the data captured during probing (CRITICAL - must arrive before user data)
            if !data_to_inject.is_empty() {
                self.send_critical_port(PortMessage::InjectData {
                    data: data_to_inject,
                })?;
            }
        } else {
            return Err(ActorError::Other(
                "No pending port for probe completion".into(),
            ));
        }

        Ok(())
    }

    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_variables))]
    #[cfg(target_arch = "wasm32")]
    async fn handle_reconfigure_with_port(
        &mut self,
        baud: u32,
        framing: String,
        port_handle: Option<actor_runtime::channels::PortHandle>,
    ) -> Result<(), ActorError> {
        if self.state != ConnectionState::Connected {
            return Err(ActorError::UnexpectedMessage {
                state: format!("{:?}", self.state),
                message: "Reconfigure".into(),
            });
        }

        // Store reconfigure parameters for event-driven completion
        // (will be processed when ConnectionClosed is received)
        self.pending_reconfigure_baud = Some(baud);
        self.pending_reconfigure_framing = Some(framing);

        // Use provided port_handle or fall back to stored one
        let handle = port_handle.or_else(|| self.pending_port_handle.clone());
        self.pending_port_handle = handle;

        // Transition to Disconnecting
        self.transition(ConnectionState::Disconnecting)?;

        // Tell PortActor to close (CRITICAL - must succeed for event-driven coordination)
        // Event-driven coordination: StateActor will receive ConnectionClosed
        // message from PortActor when close is complete, then proceed with reconnection
        self.send_critical_port(PortMessage::Close)?;

        Ok(())
    }

    /// Complete reconfiguration after port close (called from ConnectionClosed handler)
    #[cfg(target_arch = "wasm32")]
    async fn complete_reconfigure(&mut self) -> Result<(), ActorError> {
        // Extract stored parameters
        let baud = self.pending_reconfigure_baud.take().unwrap_or(0);
        let framing = self
            .pending_reconfigure_framing
            .take()
            .unwrap_or_else(|| "8N1".to_string());

        if let Some(port) = self.pending_port_handle.clone() {
            // Extract port info from SerialPort object
            let info = if let Ok(func_val) = js_sys::Reflect::get(&port, &"getInfo".into()) {
                if let Ok(func) = func_val.dyn_into::<js_sys::Function>() {
                    func.call0(&port)
                        .unwrap_or(wasm_bindgen::JsValue::from(js_sys::Object::new()))
                } else {
                    js_sys::Object::new().into()
                }
            } else {
                js_sys::Object::new().into()
            };

            let vid = js_sys::Reflect::get(&info, &"usbVendorId".into())
                .ok()
                .and_then(|v| v.as_f64())
                .map(|v| v as u16);
            let pid = js_sys::Reflect::get(&info, &"usbProductId".into())
                .ok()
                .and_then(|v| v.as_f64())
                .map(|v| v as u16);

            let port_info = actor_protocol::SerialPortInfo {
                path: format!("{:04X}:{:04X}", vid.unwrap_or(0), pid.unwrap_or(0)),
                vid,
                pid,
            };

            if baud == 0 {
                // Auto-detect requested - transition to Probing
                actor_debug!("StateActor: Reconfigure with Auto (baud=0) - starting probing");

                self.pending_port = Some(port_info.clone());
                self.pending_baud = 0;
                self.pending_port_handle = Some(port.clone());
                self.transition(ConnectionState::Probing)?;
                self.send_critical_probe(ProbeMessage::Start {
                    port: port_info,
                    port_handle: port,
                })?;
            } else {
                // Direct connection with specified baud
                actor_debug!("StateActor: Reconfigure to {}@{}", framing, baud);

                self.pending_port = Some(port_info.clone());
                self.pending_baud = baud;
                self.pending_port_handle = Some(port.clone());
                self.transition(ConnectionState::Connecting)?;
                let operation_id = self.next_operation_id();
                self.send_critical_port(PortMessage::Open {
                    port: port_info,
                    baud,
                    framing,
                    send_wakeup: false, // Reconfigure is manual, don't send wakeup
                    operation_id,
                    port_handle: port,
                })?;
            }
        }

        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    async fn handle_reconfigure(&mut self, baud: u32, framing: String) -> Result<(), ActorError> {
        if self.state != ConnectionState::Connected {
            return Err(ActorError::UnexpectedMessage {
                state: format!("{:?}", self.state),
                message: "Reconfigure".into(),
            });
        }

        // Transition to Disconnecting
        self.transition(ConnectionState::Disconnecting)?;

        // Tell PortActor to close (CRITICAL)
        self.send_critical_port(PortMessage::Close)?;

        // Transition to Disconnected
        self.transition(ConnectionState::Disconnected)?;

        // Native implementation (without WASM-specific port handling)
        if let Some(port_info) = self.pending_port.clone() {
            if baud == 0 {
                self.pending_port = Some(port_info.clone());
                self.pending_baud = 0;
                self.transition(ConnectionState::Probing)?;
                self.send_critical_probe(ProbeMessage::Start { port: port_info })?;
            } else {
                self.pending_port = Some(port_info.clone());
                self.pending_baud = baud;
                self.transition(ConnectionState::Connecting)?;
                let operation_id = self.next_operation_id();
                self.send_critical_port(PortMessage::Open {
                    port: port_info,
                    baud,
                    framing,
                    send_wakeup: false,
                    operation_id,
                })?;
            }
        }

        Ok(())
    }
}

impl Actor for StateActor {
    type Message = StateMessage;

    fn name(&self) -> &'static str {
        "StateActor"
    }

    async fn handle(&mut self, msg: StateMessage) -> Result<(), ActorError> {
        match msg {
            // UiCommand with port handle (WASM-only)
            #[cfg(target_arch = "wasm32")]
            StateMessage::UiCommandWithPort { cmd, port_handle } => match cmd {
                UiCommand::Connect {
                    port,
                    baud,
                    framing,
                } => {
                    self.handle_connect(port, baud, framing, Some(port_handle))
                        .await?
                }
                UiCommand::Reconfigure { baud, framing } => {
                    self.handle_reconfigure_with_port(baud, framing, Some(port_handle))
                        .await?
                }
                UiCommand::Disconnect => self.handle_disconnect().await?,
                UiCommand::SetDecoder { .. } | UiCommand::SetFramer { .. } => {
                    // These are handled by worker, not state machine
                }
                UiCommand::WriteData { .. } => {
                    // This should be routed directly to PortActor
                    // Not handled by StateActor
                }
            },

            // UiCommand without port handle
            StateMessage::UiCommand(cmd) => match cmd {
                UiCommand::Connect {
                    port,
                    baud,
                    framing,
                } => {
                    #[cfg(target_arch = "wasm32")]
                    {
                        self.handle_connect(port, baud, framing, None).await?
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    {
                        self.handle_connect(port, baud, framing).await?
                    }
                }
                UiCommand::Disconnect => self.handle_disconnect().await?,
                UiCommand::Reconfigure { baud, framing } => {
                    #[cfg(target_arch = "wasm32")]
                    {
                        self.handle_reconfigure_with_port(baud, framing, None)
                            .await?
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    {
                        self.handle_reconfigure(baud, framing).await?
                    }
                }
                UiCommand::SetDecoder { .. } | UiCommand::SetFramer { .. } => {
                    // These are handled by worker, not state machine
                }
                UiCommand::WriteData { .. } => {
                    // This should be routed directly to PortActor
                    // Not handled by StateActor
                }
            },
            StateMessage::ConnectionEstablished { operation_id } => {
                self.handle_connection_established(operation_id).await?
            }
            StateMessage::ConnectionFailed { reason } => {
                self.handle_connection_failed(reason).await?
            }
            StateMessage::ConnectionLost => {
                // Device disconnected - close the port cleanly before transitioning (CRITICAL)
                self.send_critical_port(PortMessage::Close)?;

                // Transition to DeviceLost state (ready for auto-reconnect)
                self.transition(ConnectionState::DeviceLost)?;
            }
            StateMessage::ConnectionClosed => {
                // PortActor has confirmed port is fully closed
                if self.state == ConnectionState::Disconnecting {
                    // Check if this is a reconfigure operation
                    #[cfg(target_arch = "wasm32")]
                    {
                        if self.pending_reconfigure_baud.is_some() {
                            // Complete reconfiguration (reconnect with new settings)
                            self.transition(ConnectionState::Disconnected)?;
                            actor_debug!("StateActor: Port closed, completing reconfiguration");
                            self.complete_reconfigure().await?;
                            return Ok(());
                        }
                    }

                    // Normal disconnect (no reconfigure)
                    self.transition(ConnectionState::Disconnected)?;
                    actor_debug!("StateActor: Port close confirmed, transitioned to Disconnected");
                } else {
                    actor_debug!(
                        "StateActor: Ignoring ConnectionClosed in {:?} state",
                        self.state
                    );
                }
            }
            StateMessage::ProbeComplete {
                baud,
                framing,
                protocol,
                initial_data,
            } => {
                self.handle_probe_complete(baud, framing, protocol, initial_data)
                    .await?
            }
            #[cfg(target_arch = "wasm32")]
            StateMessage::DeviceReappeared { port, port_handle } => {
                // Only reconnect from DeviceLost state (not from other states)
                if self.state != ConnectionState::DeviceLost {
                    actor_debug!("Ignoring DeviceReappeared in {:?} state", self.state);
                    return Ok(());
                }

                // Transition to AutoReconnecting
                self.transition(ConnectionState::AutoReconnecting)?;

                // Brief delay for port to stabilize (reduced since ReconnectActor already waited)
                use wasm_bindgen_futures::JsFuture;
                let _ = JsFuture::from(js_sys::Promise::new(&mut |resolve, _| {
                    if let Some(window) = web_sys::window() {
                        let _ = window
                            .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 50);
                    }
                }))
                .await;

                // Determine baud rate: use stored rate, or default to 115200
                let baud = if self.pending_baud > 0 {
                    self.pending_baud
                } else {
                    115200
                };

                actor_debug!("Auto-reconnecting at {} baud", baud);

                let operation_id = self.next_operation_id();

                // Trigger actual reconnection by opening the port (CRITICAL)
                // Note: AutoReconnecting → Connected transition happens when PortActor confirms success
                self.send_critical_port(PortMessage::Open {
                    port,
                    baud,
                    framing: "8N1".into(),
                    send_wakeup: false, // Auto-reconnect: don't send wakeup byte
                    operation_id,
                    port_handle,
                })?;
            }

            #[cfg(not(target_arch = "wasm32"))]
            StateMessage::DeviceReappeared { port } => {
                // Only reconnect from DeviceLost state (not from other states)
                if self.state != ConnectionState::DeviceLost {
                    return Ok(());
                }

                // Transition to AutoReconnecting
                self.transition(ConnectionState::AutoReconnecting)?;

                // Determine baud rate: use stored rate, or default to 115200
                let baud = if self.pending_baud > 0 {
                    self.pending_baud
                } else {
                    115200
                };

                let operation_id = self.next_operation_id();

                // Trigger actual reconnection by opening the port (CRITICAL)
                self.send_critical_port(PortMessage::Open {
                    port,
                    baud,
                    framing: "8N1".into(),
                    send_wakeup: false,
                    operation_id,
                })?;
            }

            StateMessage::OperationTimeout {
                operation,
                state: expected_state,
            } => {
                // Only handle timeout if we're still in the expected state
                // (if state has changed, the operation already completed)
                if self.state != expected_state {
                    actor_debug!(
                        "Ignoring {} timeout - already transitioned to {:?}",
                        operation,
                        self.state
                    );
                    return Ok(());
                }

                actor_info!("Operation timeout: {} in state {:?}", operation, self.state);

                // Send error event to UI (non-critical)
                let error_msg = format!("{} operation timed out. Please try again.", operation);
                self.send_ui_event(SystemEvent::Error { message: error_msg });

                // Transition to safe state based on current state
                match self.state {
                    ConnectionState::Probing
                    | ConnectionState::Connecting
                    | ConnectionState::AutoReconnecting => {
                        // Failed to establish connection - go to Disconnected (CRITICAL operations)
                        self.send_critical_port(PortMessage::Close)?;
                        self.transition(ConnectionState::Disconnecting)?;
                        // Note: Will transition to Disconnected when ConnectionClosed arrives
                    }
                    ConnectionState::Disconnecting => {
                        // Force transition to Disconnected even if close didn't confirm
                        actor_info!("Forcing transition to Disconnected after disconnect timeout");
                        self.transition(ConnectionState::Disconnected)?;
                    }
                    _ => {
                        // Shouldn't happen (timeout only spawned for specific states)
                        actor_info!("Unexpected timeout in state {:?}", self.state);
                    }
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use futures::stream::StreamExt;

    fn create_test_actor() -> (
        StateActor,
        mpsc::Receiver<PortMessage>,
        mpsc::Receiver<ProbeMessage>,
        mpsc::Receiver<ReconnectMessage>,
        mpsc::Receiver<SystemEvent>,
    ) {
        let (port_tx, port_rx) = mpsc::channel(100);
        let (probe_tx, probe_rx) = mpsc::channel(100);
        let (reconnect_tx, reconnect_rx) = mpsc::channel(100);
        let (event_tx, event_rx) = mpsc::channel(100);
        let (state_tx, _state_rx) = mpsc::channel(100);

        let actor = StateActor::new(port_tx, probe_tx, reconnect_tx, event_tx, state_tx);
        (actor, port_rx, probe_rx, reconnect_rx, event_rx)
    }

    #[tokio::test]
    async fn test_initial_state() {
        let (actor, _, _, _, _) = create_test_actor();
        assert_eq!(actor.state, ConnectionState::Disconnected);
    }

    #[tokio::test]
    async fn test_connect_with_baud() {
        let (mut actor, mut port_rx, _, _, mut event_rx) = create_test_actor();

        let port = actor_protocol::SerialPortInfo::new("/dev/ttyUSB0".into(), None, None);
        actor
            .handle_connect(port.clone(), 115200, "8N1".to_string())
            .await
            .unwrap();

        // Should transition to Connecting
        assert_eq!(actor.state, ConnectionState::Connecting);

        // Should send Open to PortActor
        let port_msg = port_rx.next().await.unwrap();
        match port_msg {
            PortMessage::Open { baud, .. } => assert_eq!(baud, 115200),
            _ => panic!("Wrong message"),
        }

        // Should emit state change event
        let event = event_rx.next().await.unwrap();
        match event {
            SystemEvent::StateChanged { state } => {
                assert_eq!(state, ConnectionState::Connecting);
            }
            _ => panic!("Wrong event"),
        }
    }

    #[tokio::test]
    async fn test_connect_with_auto_detect() {
        let (mut actor, _, mut probe_rx, _, _) = create_test_actor();

        let port = actor_protocol::SerialPortInfo::new("/dev/ttyUSB0".into(), None, None);
        actor
            .handle_connect(port.clone(), 0, "Auto".to_string())
            .await
            .unwrap();

        // Should transition to Probing
        assert_eq!(actor.state, ConnectionState::Probing);

        // Should send Start to ProbeActor
        let probe_msg = probe_rx.next().await.unwrap();
        match probe_msg {
            ProbeMessage::Start { .. } => {}
            _ => panic!("Wrong message"),
        }
    }

    #[tokio::test]
    async fn test_disconnect_from_connected() {
        let (mut actor, mut port_rx, _, _, _) = create_test_actor();

        // Manually set to connected
        actor.state = ConnectionState::Connected;

        actor.handle_disconnect().await.unwrap();

        // Should send Close to PortActor
        let port_msg = port_rx.next().await.unwrap();
        match port_msg {
            PortMessage::Close => {}
            _ => panic!("Wrong message"),
        }

        // Should be in Disconnecting state (event-driven coordination)
        assert_eq!(actor.state, ConnectionState::Disconnecting);

        // Simulate PortActor confirming closure
        actor.handle(StateMessage::ConnectionClosed).await.unwrap();

        // Now should be disconnected
        assert_eq!(actor.state, ConnectionState::Disconnected);
    }

    #[tokio::test]
    async fn test_connection_established() {
        let (mut actor, _, _, _, mut event_rx) = create_test_actor();

        actor.state = ConnectionState::Connecting;
        actor.operation_sequence = 1; // Simulate operation ID
        actor.handle_connection_established(1).await.unwrap();

        assert_eq!(actor.state, ConnectionState::Connected);

        // Should emit state change
        let event = event_rx.next().await.unwrap();
        match event {
            SystemEvent::StateChanged { state } => {
                assert_eq!(state, ConnectionState::Connected);
            }
            _ => panic!("Wrong event"),
        }
    }

    #[tokio::test]
    async fn test_connection_failed() {
        let (mut actor, _, _, _, mut event_rx) = create_test_actor();

        actor.state = ConnectionState::Connecting;
        actor
            .handle_connection_failed("Port busy".into())
            .await
            .unwrap();

        assert_eq!(actor.state, ConnectionState::Disconnected);

        // Should emit error event
        let event = event_rx.next().await.unwrap();
        match event {
            SystemEvent::Error { message } => {
                assert!(message.contains("Port busy"));
            }
            _ => panic!("Wrong event"),
        }
    }

    #[tokio::test]
    async fn test_connection_established_rejects_stale_operation() {
        let (mut actor, mut port_rx, _, _, _) = create_test_actor();

        actor.state = ConnectionState::Connecting;
        actor.operation_sequence = 5; // Expect operation ID 5

        // Try to establish connection with stale operation ID
        let result = actor.handle_connection_established(3).await;

        // Should reject stale operation
        assert!(result.is_err());
        assert_eq!(actor.state, ConnectionState::Connecting); // State unchanged

        // Should send Close message to PortActor to close orphan port
        let msg = port_rx.next().await.unwrap();
        match msg {
            PortMessage::Close => {} // Expected
            _ => panic!("Expected Close message, got {:?}", msg),
        }
    }

    #[tokio::test]
    async fn test_invalid_transition_rejected() {
        let (mut actor, _, _, _, _) = create_test_actor();

        actor.state = ConnectionState::Disconnected;

        // Cannot go directly to Connected
        let result = actor.transition(ConnectionState::Connected);
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_probe_complete() {
        let (mut actor, mut port_rx, _, _, mut event_rx) = create_test_actor();

        actor.state = ConnectionState::Probing;
        // Set pending port (as would happen during connect)
        actor.pending_port = Some(actor_protocol::SerialPortInfo::new(
            "/dev/ttyUSB0".into(),
            None,
            None,
        ));

        actor
            .handle_probe_complete(115200, "8N1".into(), Some("mavlink".into()), vec![1, 2, 3])
            .await
            .unwrap();

        assert_eq!(actor.state, ConnectionState::Connecting);

        // Should have sent Open to PortActor
        let port_msg = port_rx.next().await.unwrap();
        match port_msg {
            PortMessage::Open { baud, framing, .. } => {
                assert_eq!(baud, 115200);
                assert_eq!(framing, "8N1");
            }
            _ => panic!("Wrong message"),
        }

        // Should emit status update
        let event = event_rx.next().await.unwrap();
        match event {
            SystemEvent::StatusUpdate { message } => {
                assert!(message.contains("mavlink"));
                assert!(message.contains("115200"));
            }
            _ => panic!("Wrong event"),
        }
    }

    #[tokio::test]
    async fn test_disconnect_aborts_probe() {
        let (mut actor, mut port_rx, mut probe_rx, _, _) = create_test_actor();

        actor.state = ConnectionState::Probing;
        actor.handle_disconnect().await.unwrap();

        // Should send Abort to ProbeActor
        let probe_msg = probe_rx.next().await.unwrap();
        match probe_msg {
            ProbeMessage::Abort => {}
            _ => panic!("Wrong message"),
        }

        // Should send Close to PortActor
        let port_msg = port_rx.next().await.unwrap();
        match port_msg {
            PortMessage::Close => {}
            _ => panic!("Expected Close message"),
        }

        // Should be in Disconnecting state (event-driven coordination)
        assert_eq!(actor.state, ConnectionState::Disconnecting);

        // Simulate PortActor confirming closure
        actor.handle(StateMessage::ConnectionClosed).await.unwrap();

        // Now should be disconnected
        assert_eq!(actor.state, ConnectionState::Disconnected);
    }

    #[tokio::test]
    async fn test_unexpected_message_error() {
        let (mut actor, _, _, _, _) = create_test_actor();

        // Try to connect when already connecting
        actor.state = ConnectionState::Connecting;
        let port = actor_protocol::SerialPortInfo::new("/dev/ttyUSB0".into(), None, None);
        let result: Result<(), ActorError> =
            actor.handle_connect(port, 115200, "8N1".to_string()).await;

        assert!(result.is_err());
        match result.unwrap_err() {
            ActorError::UnexpectedMessage { state, message } => {
                assert!(state.contains("Connecting"));
                assert_eq!(message, "Connect");
            }
            _ => panic!("Wrong error type"),
        }
    }

    #[tokio::test]
    async fn test_probe_complete_ignored_when_disconnected() {
        // Fix #9 verification
        let (mut actor, _, _, _, _) = create_test_actor();

        // State is Disconnected
        actor.state = ConnectionState::Disconnected;

        // Simulate delayed ProbeComplete arriving after disconnect
        let result: Result<(), ActorError> = actor
            .handle_probe_complete(115200, "8N1".into(), None, vec![])
            .await;

        // Should be Ok(()) (ignored), not Err
        assert!(result.is_ok());
        assert_eq!(actor.state, ConnectionState::Disconnected);
    }

    #[tokio::test]
    async fn test_device_reappeared_triggers_reconnection() {
        let (mut actor, mut port_rx, _, _, mut event_rx) = create_test_actor();

        // Simulate device lost
        actor.state = ConnectionState::DeviceLost;
        actor.pending_baud = 115200;

        // Simulate device reappearing
        let port =
            actor_protocol::SerialPortInfo::new("/dev/ttyUSB0".into(), Some(0x1234), Some(0x5678));
        actor
            .handle(StateMessage::DeviceReappeared { port: port.clone() })
            .await
            .unwrap();

        // Should transition to AutoReconnecting
        let event = event_rx.next().await.unwrap();
        match event {
            SystemEvent::StateChanged { state } => {
                assert_eq!(state, ConnectionState::AutoReconnecting);
            }
            _ => panic!("Wrong event"),
        }

        // Should send Open to PortActor
        let port_msg = port_rx.next().await.unwrap();
        match port_msg {
            PortMessage::Open { port: p, baud, .. } => {
                assert_eq!(p.path, "/dev/ttyUSB0");
                assert_eq!(baud, 115200);
            }
            _ => panic!("Wrong message"),
        }

        // State should still be AutoReconnecting (waits for ConnectionEstablished)
        assert_eq!(actor.state, ConnectionState::AutoReconnecting);
    }

    #[tokio::test]
    async fn test_device_reappeared_ignored_when_not_device_lost() {
        let (mut actor, mut port_rx, _, _, _) = create_test_actor();

        // State is Disconnected (not DeviceLost)
        actor.state = ConnectionState::Disconnected;

        let port = actor_protocol::SerialPortInfo::new("/dev/ttyUSB0".into(), None, None);
        actor
            .handle(StateMessage::DeviceReappeared { port })
            .await
            .unwrap();

        // Should NOT send any messages to PortActor
        assert!(port_rx.try_next().is_err());

        // State should remain Disconnected
        assert_eq!(actor.state, ConnectionState::Disconnected);
    }

    #[tokio::test]
    async fn test_device_reappeared_uses_default_baud_when_not_set() {
        let (mut actor, mut port_rx, _, _, _) = create_test_actor();

        // Simulate device lost with no pending_baud set
        actor.state = ConnectionState::DeviceLost;
        actor.pending_baud = 0; // Not set

        let port =
            actor_protocol::SerialPortInfo::new("/dev/ttyUSB0".into(), Some(0x1234), Some(0x5678));
        actor
            .handle(StateMessage::DeviceReappeared { port })
            .await
            .unwrap();

        // Should use default 115200 baud
        let port_msg = port_rx.next().await.unwrap();
        match port_msg {
            PortMessage::Open { baud, .. } => {
                assert_eq!(baud, 115200); // Default baud rate
            }
            _ => panic!("Wrong message"),
        }
    }

    #[tokio::test]
    async fn test_connection_lost_closes_port() {
        let (mut actor, mut port_rx, _, _, mut event_rx) = create_test_actor();

        // Simulate connected state
        actor.state = ConnectionState::Connected;

        // Handle connection lost (USB disconnect)
        actor.handle(StateMessage::ConnectionLost).await.unwrap();

        // Should send Close to PortActor to clean up
        let port_msg = port_rx.next().await.unwrap();
        match port_msg {
            PortMessage::Close => {}
            _ => panic!("Expected Close message, got {:?}", port_msg),
        }

        // Should transition to DeviceLost
        let event = event_rx.next().await.unwrap();
        match event {
            SystemEvent::StateChanged { state } => {
                assert_eq!(state, ConnectionState::DeviceLost);
            }
            _ => panic!("Wrong event"),
        }

        assert_eq!(actor.state, ConnectionState::DeviceLost);
    }

    #[tokio::test]
    async fn test_connection_established_from_auto_reconnecting() {
        let (mut actor, _, _, _, mut event_rx) = create_test_actor();

        // Simulate auto-reconnecting state (after USB replug)
        actor.state = ConnectionState::AutoReconnecting;
        actor.pending_port = Some(actor_protocol::SerialPortInfo::new(
            "/dev/ttyUSB0".into(),
            Some(0x1234),
            Some(0x5678),
        ));
        actor.pending_baud = 115200;
        actor.operation_sequence = 1; // Simulate operation ID

        // Port opens successfully and sends ConnectionEstablished
        actor.handle_connection_established(1).await.unwrap();

        // Should transition to Connected
        assert_eq!(actor.state, ConnectionState::Connected);

        // Should emit state change event
        let event = event_rx.next().await.unwrap();
        match event {
            SystemEvent::StateChanged { state } => {
                assert_eq!(state, ConnectionState::Connected);
            }
            _ => panic!("Wrong event"),
        }
    }

    #[tokio::test]
    async fn test_operation_timeout_connecting() {
        let (mut actor, mut port_rx, _, _, mut event_rx) = create_test_actor();

        // Transition to Connecting
        actor.state = ConnectionState::Connecting;

        // Simulate timeout message
        actor
            .handle(StateMessage::OperationTimeout {
                operation: "Connecting".to_string(),
                state: ConnectionState::Connecting,
            })
            .await
            .unwrap();

        // Should send error event
        let event = event_rx.next().await.unwrap();
        match event {
            SystemEvent::Error { message } => {
                assert!(message.contains("Connecting"));
                assert!(message.contains("timed out"));
            }
            _ => panic!("Expected Error event, got {:?}", event),
        }

        // Should send Close to PortActor
        let port_msg = port_rx.next().await.unwrap();
        match port_msg {
            PortMessage::Close => {}
            _ => panic!("Expected Close message"),
        }

        // Should transition to Disconnecting
        assert_eq!(actor.state, ConnectionState::Disconnecting);
    }

    #[tokio::test]
    async fn test_operation_timeout_ignored_after_state_change() {
        let (mut actor, _, _, _, mut event_rx) = create_test_actor();

        // Transition to Connecting, then immediately to Connected
        actor.state = ConnectionState::Connected;

        // Simulate timeout message for old state
        actor
            .handle(StateMessage::OperationTimeout {
                operation: "Connecting".to_string(),
                state: ConnectionState::Connecting,
            })
            .await
            .unwrap();

        // Timeout should be ignored (no error event)
        // Consume state change events from previous transitions
        while let Ok(Some(event)) = event_rx.try_next() {
            if let SystemEvent::Error { .. } = event {
                panic!("Should not send error for stale timeout");
            }
            // Ignore other events
        }

        // Should remain in Connected state
        assert_eq!(actor.state, ConnectionState::Connected);
    }

    #[tokio::test]
    async fn test_operation_timeout_disconnecting() {
        let (mut actor, _, _, _, mut event_rx) = create_test_actor();

        // Transition to Disconnecting
        actor.state = ConnectionState::Disconnecting;

        // Simulate timeout message
        actor
            .handle(StateMessage::OperationTimeout {
                operation: "Disconnecting".to_string(),
                state: ConnectionState::Disconnecting,
            })
            .await
            .unwrap();

        // Should send error event
        let event = event_rx.next().await.unwrap();
        match event {
            SystemEvent::Error { message } => {
                assert!(message.contains("Disconnecting"));
            }
            _ => panic!("Expected Error event"),
        }

        // Should force transition to Disconnected
        assert_eq!(actor.state, ConnectionState::Disconnected);
    }
}
