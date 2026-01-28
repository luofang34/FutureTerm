// Active FSM Driver Constants
pub const STATE_MASK: u8 = 0x7F; // 0b01111111
pub const LOCK_FLAG: u8 = 0x80; // 0b10000000

/// # Connection State Machine
///
/// This module implements a unified state machine for managing serial port connections.
/// The state machine prevents race conditions, invalid state combinations, and provides
/// a single source of truth for connection status.
///
/// ## State Transition Diagram
///
/// ```text
///                         ┌─────────────────┐
///                    ┌───►│  Disconnected   │◄────────────┐
///                    │    └────┬────────┬───┘             │
///                    │         │        │                 │
///         User       │   User  │        │ User            │ Disconnect
///         Cancel     │   Auto  │        │ Manual          │ Complete
///                    │         │        │                 │
///              ┌─────┴────┐   │   ┌────▼───────┐    ┌────┴──────────┐
///         ┌───►│ Probing  │   │   │ Connecting │◄───┤ Disconnecting │◄────┐
///         │    └────┬─────┘   │   └─────┬──────┘    └───────────────┘     │
///         │         │         │         │                                  │
///         │    Probe│         │    Port │                                  │
///         │   Complete       │    Opens│                             User  │
///         │         │         │         │                           Disconnect
///         │         └─────────┴─────────┤                                  │
///         │                              │                                  │
///         │                         ┌────▼────────┐    ┌──────────────┐    │
///         │   USB                   │  Connected  │───►│ Reconfiguring│────┘
///         │  Hotplug           ┌───►└─────────────┘    └──────────────┘
///         │  Event             │           │               Baud Change
///         │  (w/ VID/PID)      │           │ USB                  │
///         │                    │           │ Unplug               │
///         │               ┌────┴───────┐   │                      │
///         │               │ Device     │◄──┴──────────────────────┘
///         │          ┌───►│ Lost       │
///         │          │    └────┬───────┘
///         │     Failed    │    │
///         │  Reconnect    │    │ Device
///         │               │    │ Reappears
///         │    ┌──────────┴────▼────────────┐
///         └────┤   AutoReconnecting         │
///              └────────────────────────────┘
/// ```
///
/// ## State Invariants
///
/// - **Disconnected**: No port open, no actors running, ready for new connection
/// - **Probing**: Port open (read-only), ProbeActor running baud detection
/// - **Connecting**: Port opening, PortActor waiting for open() confirmation
/// - **Connected**: Port open, read loop running, ReconnectActor monitoring USB events
/// - **Reconfiguring**: Port closing, will reopen with new settings (event-driven)
/// - **Disconnecting**: Port closing, waiting for PortActor confirmation (event-driven)
/// - **DeviceLost**: Port closed, ReconnectActor polling for device reappearance
/// - **AutoReconnecting**: Port opening after device reappeared, attempting reconnection
///
/// ## Transition Triggers
///
/// ### User Actions
/// - **Connect (manual baud)**: Disconnected → Connecting
/// - **Connect (auto-detect)**: Disconnected → Probing
/// - **Disconnect**: Any active state → Disconnecting → Disconnected
/// - **Reconfigure**: Connected → Reconfiguring → Connecting → Connected
///
/// ### System Events
/// - **Port opens successfully**: Connecting → Connected
/// - **Port open fails**: Connecting → Disconnected
/// - **Probe completes**: Probing → Connecting → Connected
/// - **Probe fails**: Probing → Disconnected
/// - **USB unplug (with VID/PID)**: Connected → DeviceLost → AutoReconnecting → Connected
/// - **USB unplug (no VID/PID)**: Connected → DeviceLost (waits for user action)
/// - **Reconnect succeeds**: AutoReconnecting → Connected
/// - **Reconnect fails**: AutoReconnecting → DeviceLost
///
/// ## Event-Driven Coordination
///
/// Two operations use event-driven coordination to avoid arbitrary delays:
/// - **Disconnect**: Disconnecting → (wait for ConnectionClosed) → Disconnected
/// - **Reconfigure**: Reconfiguring → Disconnecting → (wait for ConnectionClosed) → Connecting
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum ConnectionState {
    /// No active connection, ready to connect
    Disconnected,

    /// Actively probing ports to detect firmware
    Probing,

    /// Initiating connection to port
    Connecting,

    /// Successfully connected and operational
    Connected,

    /// Changing baud rate or serial parameters
    Reconfiguring,

    /// User clicked disconnect, tearing down connection
    Disconnecting,

    /// Device physically disconnected, waiting for user action
    DeviceLost,

    /// Device physically disconnected, attempting to reconnect
    AutoReconnecting,
}

impl ConnectionState {
    /// Can the user trigger a disconnect action?
    pub fn can_disconnect(&self) -> bool {
        matches!(
            self,
            Self::Probing
                | Self::Connecting
                | Self::Connected
                | Self::AutoReconnecting
                | Self::DeviceLost
        )
    }

    /// Should the button show "Disconnect"?
    pub fn button_shows_disconnect(&self) -> bool {
        self.can_disconnect()
    }

    /// What color should the status indicator be?
    pub fn indicator_color(&self) -> &'static str {
        match self {
            Self::Connected => "rgb(95, 200, 85)",         // Green
            Self::Disconnected => "rgb(240, 105, 95)",     // Red
            Self::DeviceLost => "rgb(245, 190, 80)",       // Orange (pulsing - waiting for device)
            Self::AutoReconnecting => "rgb(245, 190, 80)", // Orange (pulsing)
            Self::Connecting | Self::Probing | Self::Reconfiguring | Self::Disconnecting => {
                "rgb(245, 190, 80)" // Orange (steady)
            }
        }
    }

    /// Should the indicator pulse?
    pub fn indicator_should_pulse(&self) -> bool {
        matches!(self, Self::AutoReconnecting | Self::DeviceLost)
    }

    /// User-facing status text
    pub fn status_text(&self) -> &'static str {
        match self {
            Self::Disconnected => "Ready to connect",
            Self::Probing => "Detecting firmware...",
            Self::Connecting => "Connecting...",
            Self::Connected => "Connected",
            Self::Reconfiguring => "Reconfiguring...",
            Self::Disconnecting => "Disconnecting...",
            Self::DeviceLost => "Device Lost (waiting for device...)",
            Self::AutoReconnecting => "Device found. Auto-reconnecting...",
        }
    }

    /// Validate if transition to new_state is allowed from current state
    /// This provides compile-time safety via exhaustive match and runtime validation
    pub fn can_transition_to(&self, new_state: ConnectionState) -> bool {
        use ConnectionState::*;

        match (self, new_state) {
            // From Disconnected
            (Disconnected, Probing) => true, // User starts auto-detect
            (Disconnected, Connecting) => true, // User connects with specific baud
            (Disconnected, Disconnected) => true, // Idempotent (no-op)

            // From Probing
            (Probing, Connecting) => true, // Probe complete, connecting
            (Probing, Disconnecting) => true, // User cancels probe
            (Probing, Disconnected) => true, // Probe failed/canceled
            (Probing, AutoReconnecting) => true, // Device found during probe (onconnect race)

            // From Connecting
            (Connecting, Connected) => true, // Connection successful
            (Connecting, Disconnecting) => true, // User cancels connection
            (Connecting, Disconnected) => true, // Connection failed
            (Connecting, DeviceLost) => true, // Device unplugged during connect

            // From Connected
            (Connected, Reconfiguring) => true, // Baud rate change
            (Connected, Disconnecting) => true, // User disconnect
            (Connected, DeviceLost) => true,    // USB unplugged (no auto-reconnect)
            (Connected, AutoReconnecting) => true, // USB unplugged (has VID/PID)

            // From Reconfiguring
            (Reconfiguring, Connected) => true, // Reconfig complete
            (Reconfiguring, DeviceLost) => true, // USB unplugged during reconfig

            // From Disconnecting
            (Disconnecting, Disconnected) => true, // Disconnect complete

            // From DeviceLost
            (DeviceLost, Disconnected) => true,  // User gives up
            (DeviceLost, Disconnecting) => true, // User initiates disconnect
            (DeviceLost, AutoReconnecting) => true, // Device replugged
            (DeviceLost, Connecting) => true,    // Manual reconnect attempt
            (DeviceLost, DeviceLost) => true,    // Idempotent (race condition safety)

            // From AutoReconnecting
            (AutoReconnecting, Connected) => true, // Reconnect successful
            (AutoReconnecting, DeviceLost) => true, // Reconnect failed
            (AutoReconnecting, Disconnected) => true, // User cancels
            (AutoReconnecting, Disconnecting) => true, // User initiates disconnect
            (AutoReconnecting, AutoReconnecting) => true, // Idempotent retry

            // All other transitions are invalid
            _ => false,
        }
    }

    /// Convert state to u8 value for atomic storage
    /// Returns value in range 0-7 (uses lower 3 bits)
    pub fn to_u8(self) -> u8 {
        match self {
            ConnectionState::Disconnected => 0,
            ConnectionState::Probing => 1,
            ConnectionState::Connecting => 2,
            ConnectionState::Connected => 3,
            ConnectionState::Reconfiguring => 4,
            ConnectionState::Disconnecting => 5,
            ConnectionState::DeviceLost => 6,
            ConnectionState::AutoReconnecting => 7,
        }
    }

    /// Convert u8 value back to state (masks out lock bit)
    /// Returns None if value is invalid
    pub fn from_u8(value: u8) -> Option<Self> {
        match value & STATE_MASK {
            0 => Some(ConnectionState::Disconnected),
            1 => Some(ConnectionState::Probing),
            2 => Some(ConnectionState::Connecting),
            3 => Some(ConnectionState::Connected),
            4 => Some(ConnectionState::Reconfiguring),
            5 => Some(ConnectionState::Disconnecting),
            6 => Some(ConnectionState::DeviceLost),
            7 => Some(ConnectionState::AutoReconnecting),
            _ => None,
        }
    }
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn test_state_conversion_roundtrip() {
        let states = [
            ConnectionState::Disconnected,
            ConnectionState::Probing,
            ConnectionState::Connecting,
            ConnectionState::Connected,
            ConnectionState::Reconfiguring,
            ConnectionState::Disconnecting,
            ConnectionState::DeviceLost,
            ConnectionState::AutoReconnecting,
        ];

        for state in states {
            let u8_val = state.to_u8();
            let recovered = ConnectionState::from_u8(u8_val).unwrap();
            assert_eq!(state, recovered);
        }
    }

    #[test]
    fn test_valid_transitions() {
        assert!(ConnectionState::Disconnected.can_transition_to(ConnectionState::Connecting));
        assert!(ConnectionState::Connecting.can_transition_to(ConnectionState::Connected));
        assert!(ConnectionState::Connected.can_transition_to(ConnectionState::Disconnecting));
        assert!(ConnectionState::Disconnecting.can_transition_to(ConnectionState::Disconnected));
    }

    #[test]
    fn test_invalid_transitions() {
        // Cannot go directly from Disconnected to Connected
        assert!(!ConnectionState::Disconnected.can_transition_to(ConnectionState::Connected));

        // Cannot go from Connected to Probing
        assert!(!ConnectionState::Connected.can_transition_to(ConnectionState::Probing));
    }

    #[test]
    fn test_serialization() {
        let state = ConnectionState::Connected;
        let json = serde_json::to_string(&state).unwrap();
        let deserialized: ConnectionState = serde_json::from_str(&json).unwrap();
        assert_eq!(state, deserialized);
    }
}
