use actor_protocol::{ActorError, ConnectionState, SystemEvent};
use actor_runtime::{
    actor_debug, spawn_timeout, PortMessage, ProbeMessage, ReconnectMessage, StateMessage,
    SupervisionConfig, TimeoutHandle,
};
use futures_channel::mpsc;

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
    #[cfg(target_arch = "wasm32")]
    pending_reconfigure_active_framing: Option<String>,
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
            #[cfg(target_arch = "wasm32")]
            pending_reconfigure_active_framing: None,
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
}

mod actor_impl;
mod lifecycle;
mod reconfigure;

#[cfg(test)]
mod tests;
