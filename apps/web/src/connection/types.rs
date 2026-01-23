use futures_channel::{mpsc, oneshot};
use leptos::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use wasm_bindgen::closure::Closure;
use web_sys::Worker;

// Active FSM Driver Constants
pub const STATE_MASK: u8 = 0x7F; // 0b01111111
pub const LOCK_FLAG: u8 = 0x80; // 0b10000000

/// # Connection State Machine
///
/// This module implements a unified state machine for managing serial port connections.
/// The state machine prevents race conditions, invalid state combinations, and provides
/// a single source of truth for connection status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
            Self::Connected | Self::AutoReconnecting | Self::DeviceLost
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
            Self::DeviceLost => "rgb(245, 190, 80)",       // Orange steady (waiting for device)
            Self::AutoReconnecting => "rgb(245, 190, 80)", // Orange (pulsing)
            Self::Connecting | Self::Probing | Self::Reconfiguring | Self::Disconnecting => {
                "rgb(245, 190, 80)" // Orange (steady)
            }
        }
    }

    /// Should the indicator pulse?
    pub fn indicator_should_pulse(&self) -> bool {
        matches!(self, Self::AutoReconnecting)
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
            (Probing, Disconnected) => true, // Probe failed/canceled
            (Probing, AutoReconnecting) => true, // Device found during probe (onconnect race)

            // From Connecting
            (Connecting, Connected) => true, // Connection successful
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
            (DeviceLost, Disconnected) => true, // User gives up
            (DeviceLost, Disconnecting) => true, // User initiates disconnect
            (DeviceLost, AutoReconnecting) => true, // Device replugged
            (DeviceLost, Connecting) => true,   // Manual reconnect attempt

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

// Message passing infrastructure
pub enum ConnectionCommand {
    Stop,
    Write {
        data: Vec<u8>,
        response: oneshot::Sender<Result<(), String>>,
    },
}

pub struct ConnectionHandle {
    pub cmd_tx: mpsc::UnboundedSender<ConnectionCommand>,
    pub completion_rx: oneshot::Receiver<()>,
}

// Type alias for event handler closures to avoid clippy::type_complexity warning
pub type EventClosures = (
    Closure<dyn FnMut(web_sys::Event)>,
    Closure<dyn FnMut(web_sys::Event)>,
);

// Snapshot for UI state consistency (not true transaction rollback)
// WebSerial API doesn't support true rollback since port can't be opened twice
// This captures UI-visible state for restoration on reconfigure failure
#[derive(Clone)]
pub struct ConnectionSnapshot {
    pub baud: u32,
    pub framing: String,
    pub decoder_id: String, /* Protocol decoder (e.g., "utf8", "mavlink")
                             * Note: VID/PID not captured - they don't change during reconfiguration
                             * Note: Connection handle not captured - cannot be restored (port must
                             * be reopened) */
}

// Helper: Parse Framing String
pub fn parse_framing(s: &str) -> (u8, String, u8) {
    let chars: Vec<char> = s.chars().collect();
    let d = chars.first().and_then(|c| c.to_digit(10)).unwrap_or(8) as u8;
    let p = match chars.get(1) {
        Some('N') => "none",
        Some('E') => "even",
        Some('O') => "odd",
        _ => "none",
    }
    .to_string();
    let s_bits = chars.get(2).and_then(|c| c.to_digit(10)).unwrap_or(1) as u8;
    (d, p, s_bits)
}

#[derive(Clone)]
pub struct ConnectionManager {
    // ============ Public State Signals (for UI) ============

    // Connection State Machine (Single Source of Truth)
    pub state: Signal<ConnectionState>,
    pub set_state: WriteSignal<ConnectionState>,

    // Status text (for display purposes)
    pub status: Signal<String>,
    pub set_status: WriteSignal<String>,

    // Detected Config (for UI feedback)
    pub detected_baud: Signal<u32>,
    pub set_detected_baud: WriteSignal<u32>,
    pub detected_framing: Signal<String>,
    pub set_detected_framing: WriteSignal<String>,

    // Decoder State
    pub decoder_id: Signal<String>,
    pub set_decoder_id: WriteSignal<String>,

    // ============ Activity Indicators (transient pulses) ============
    pub rx_active: Signal<bool>,
    pub set_rx_active: WriteSignal<bool>,
    pub tx_active: Signal<bool>,
    pub set_tx_active: WriteSignal<bool>,

    // ============ Internal State (Made pub(crate) for modules) ============
    pub connection_handle: Rc<RefCell<Option<ConnectionHandle>>>,
    pub active_port: Rc<RefCell<Option<web_sys::SerialPort>>>,
    pub worker: Signal<Option<Worker>>, // Read-only access to worker for sending data
    pub last_auto_baud: Rc<RefCell<Option<u32>>>,
    // Active FSM driver (unified state + lock)
    pub atomic_state: Arc<AtomicConnectionState>,

    /// User-initiated disconnect flag (to distinguish from device loss)
    pub user_initiated_disconnect: Rc<Cell<bool>>,

    // Auto-reconnect device tracking (to clear on user disconnect)
    pub set_last_vid: Rc<RefCell<Option<WriteSignal<Option<u16>>>>>,
    pub set_last_pid: Rc<RefCell<Option<WriteSignal<Option<u16>>>>>,

    /// Track if probing was interrupted by ondisconnect events
    pub probing_interrupted: Rc<Cell<bool>>,

    // Event handler closures (to prevent memory leaks on re-initialization)
    pub event_closures: Rc<RefCell<Option<EventClosures>>>,
}

/// Active FSM driver: combines state + lock in single atomic
pub struct AtomicConnectionState {
    state: AtomicU8,
}

impl AtomicConnectionState {
    /// Create new atomic state initialized to given state (unlocked)
    pub fn new(initial: ConnectionState) -> Self {
        Self {
            state: AtomicU8::new(initial.to_u8()),
        }
    }

    /// Get current state (ignoring lock bit)
    pub fn get(&self) -> ConnectionState {
        let raw = self.state.load(Ordering::SeqCst);
        ConnectionState::from_u8(raw & STATE_MASK).unwrap_or(ConnectionState::Disconnected)
    }

    /// Check if any operation is in progress (lock bit set)
    pub fn is_locked(&self) -> bool {
        let raw = self.state.load(Ordering::SeqCst);
        (raw & LOCK_FLAG) != 0
    }

    /// Try to transition from expected state to new state with lock
    ///
    /// CRITICAL: This validates the transition is allowed by the state machine
    /// before attempting the atomic operation. This prevents invalid state
    /// transitions at the lowest level.
    pub fn try_transition(
        &self,
        from: ConnectionState,
        to: ConnectionState,
    ) -> Result<ConnectionState, ConnectionState> {
        // CRITICAL FIX: Validate transition is allowed by state machine
        if !from.can_transition_to(to) {
            // Validation failed - transition not allowed
            // (caller should log this error with context)
            return Err(from);
        }

        let from_u8 = from.to_u8();
        let to_u8_locked = to.to_u8() | LOCK_FLAG;

        match self
            .state
            .compare_exchange(from_u8, to_u8_locked, Ordering::SeqCst, Ordering::SeqCst)
        {
            Ok(_) => Ok(from),
            Err(actual) => {
                let actual_state = ConnectionState::from_u8(actual & STATE_MASK)
                    .unwrap_or(ConnectionState::Disconnected);
                Err(actual_state)
            }
        }
    }

    /// Unlock and transition to new state
    /// Must only be called by the task that holds the lock.
    pub fn unlock_and_set(&self, new_state: ConnectionState) {
        let new_u8 = new_state.to_u8();
        self.state.store(new_u8, Ordering::SeqCst);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_autoreconnecting_can_disconnect() {
        // This was the missing transition that caused the bug
        assert!(
            ConnectionState::AutoReconnecting.can_transition_to(ConnectionState::Disconnecting),
            "AutoReconnecting MUST allow transition to Disconnecting"
        );
    }

    #[test]
    fn test_try_transition_validates_state_machine() {
        let state = AtomicConnectionState::new(ConnectionState::Disconnected);

        // Invalid: Disconnected → Connected (must go through Connecting)
        let result =
            state.try_transition(ConnectionState::Disconnected, ConnectionState::Connected);

        assert!(result.is_err(), "Should reject invalid transition");
        assert_eq!(state.get(), ConnectionState::Disconnected);
        assert!(!state.is_locked());
    }

    #[test]
    fn test_all_invalid_transitions_rejected() {
        use ConnectionState::*;

        let invalid = vec![
            (Disconnected, Connected),  // Must go through Connecting
            (Connected, Disconnected),  // Must go through Disconnecting
            (Disconnecting, Connected), // Disconnecting is one-way
            (Probing, Disconnecting),   // Must cancel probing first
        ];

        for (from, to) in invalid {
            let state = AtomicConnectionState::new(from);
            let result = state.try_transition(from, to);
            assert!(result.is_err(), "{:?} → {:?} should fail", from, to);
        }
    }

    #[test]
    fn test_autoreconnecting_disconnect_sequence() {
        let state = Arc::new(AtomicConnectionState::new(
            ConnectionState::AutoReconnecting,
        ));

        // Step 1: Transition to Disconnecting
        let r1 = state.try_transition(
            ConnectionState::AutoReconnecting,
            ConnectionState::Disconnecting,
        );
        assert!(r1.is_ok(), "Should allow AutoReconnecting → Disconnecting");
        assert_eq!(state.get(), ConnectionState::Disconnecting);
        assert!(state.is_locked());

        // Step 2: Unlock and set Disconnected
        state.unlock_and_set(ConnectionState::Disconnected);
        assert_eq!(state.get(), ConnectionState::Disconnected);
        assert!(!state.is_locked());
    }

    #[test]
    fn test_bug_autoreconnecting_disconnect_no_panic() {
        use ConnectionState::*;

        // Simulate the exact bug scenario
        let state = AtomicConnectionState::new(AutoReconnecting);

        // This was causing panic before fix
        let result = state.try_transition(AutoReconnecting, Disconnecting);

        // Should succeed now that transition is valid
        assert!(
            result.is_ok(),
            "Bug regression: AutoReconnecting → Disconnecting must be valid"
        );
    }

    #[test]
    fn test_state_encoding() {
        // Test all 8 states
        for s in [
            ConnectionState::Disconnected,
            ConnectionState::Probing,
            ConnectionState::Connecting,
            ConnectionState::Connected,
            ConnectionState::Reconfiguring,
            ConnectionState::Disconnecting,
            ConnectionState::DeviceLost,
            ConnectionState::AutoReconnecting,
        ] {
            let state = AtomicConnectionState::new(s);
            assert_eq!(state.get(), s);
            assert!(!state.is_locked());
        }
    }

    #[test]
    fn test_successful_transition() {
        let state = AtomicConnectionState::new(ConnectionState::Disconnected);

        let result =
            state.try_transition(ConnectionState::Disconnected, ConnectionState::Connecting);
        assert!(result.is_ok());
        assert_eq!(state.get(), ConnectionState::Connecting);
        assert!(state.is_locked());
    }

    #[test]
    fn test_transition_while_locked() {
        let state = AtomicConnectionState::new(ConnectionState::Disconnected);

        // First transition succeeds
        state
            .try_transition(ConnectionState::Disconnected, ConnectionState::Connecting)
            .unwrap();

        // Second transition fails (still locked)
        let result = state.try_transition(ConnectionState::Connecting, ConnectionState::Connected);
        assert!(result.is_err());
    }

    #[test]
    fn test_unlock_and_set() {
        let state = AtomicConnectionState::new(ConnectionState::Disconnected);

        state
            .try_transition(ConnectionState::Disconnected, ConnectionState::Connecting)
            .unwrap();
        assert!(state.is_locked());

        state.unlock_and_set(ConnectionState::Connected);
        assert_eq!(state.get(), ConnectionState::Connected);
        assert!(!state.is_locked());
    }

    #[test]
    fn test_concurrent_transitions() {
        let state = Arc::new(AtomicConnectionState::new(ConnectionState::Disconnected));

        // First transition succeeds
        let r1 = state.try_transition(ConnectionState::Disconnected, ConnectionState::Connecting);
        assert!(r1.is_ok());

        // Concurrent attempt fails
        let r2 = state.try_transition(ConnectionState::Disconnected, ConnectionState::Connecting);
        assert!(r2.is_err());

        // After unlock, new transition succeeds
        state.unlock_and_set(ConnectionState::Connected);
        let r3 = state.try_transition(ConnectionState::Connected, ConnectionState::Disconnecting);
        assert!(r3.is_ok());
    }

    #[test]
    fn test_is_locked() {
        let state = AtomicConnectionState::new(ConnectionState::Disconnected);
        assert!(!state.is_locked());

        state
            .try_transition(ConnectionState::Disconnected, ConnectionState::Connecting)
            .unwrap();
        assert!(state.is_locked());

        state.unlock_and_set(ConnectionState::Connected);
        assert!(!state.is_locked());
    }

    // Additional tests from original implementation

    #[test]
    fn test_parse_framing_8n1() {
        let (d, p, s) = parse_framing("8N1");
        assert_eq!(d, 8);
        assert_eq!(p, "none");
        assert_eq!(s, 1);
    }

    #[test]
    fn test_parse_framing_7e1() {
        let (d, p, s) = parse_framing("7E1");
        assert_eq!(d, 7);
        assert_eq!(p, "even");
        assert_eq!(s, 1);
    }

    #[test]
    fn test_parse_framing_8o2() {
        let (d, p, s) = parse_framing("8O2");
        assert_eq!(d, 8);
        assert_eq!(p, "odd");
        assert_eq!(s, 2);
    }

    #[test]
    fn test_parse_framing_invalid() {
        // Invalid input should use defaults
        let (d, p, s) = parse_framing("X");
        assert_eq!(d, 8);
        assert_eq!(p, "none");
        assert_eq!(s, 1);
    }

    #[test]
    fn test_valid_state_transitions() {
        use ConnectionState::*;

        // Test a comprehensive set of valid transitions
        let valid = vec![
            (Disconnected, Probing),
            (Disconnected, Connecting),
            (Probing, Connecting),
            (Probing, Disconnected),
            (Connecting, Connected),
            (Connecting, Disconnected),
            (Connecting, DeviceLost),
            (Connected, Reconfiguring),
            (Connected, Disconnecting),
            (Connected, DeviceLost),
            (Connected, AutoReconnecting),
            (Reconfiguring, Connected),
            (Reconfiguring, DeviceLost),
            (Disconnecting, Disconnected),
            (DeviceLost, Disconnected),
            (DeviceLost, Disconnecting),
            (DeviceLost, AutoReconnecting),
            (DeviceLost, Connecting),
            (AutoReconnecting, Connected),
            (AutoReconnecting, DeviceLost),
            (AutoReconnecting, Disconnected),
            (AutoReconnecting, Disconnecting),
        ];

        for (from, to) in valid {
            assert!(
                from.can_transition_to(to),
                "{:?} → {:?} should be valid",
                from,
                to
            );
        }
    }

    #[test]
    fn test_state_ui_helpers() {
        use ConnectionState::*;

        // Test indicator colors
        assert_eq!(Connected.indicator_color(), "rgb(95, 200, 85)");
        assert_eq!(Disconnected.indicator_color(), "rgb(240, 105, 95)");
        assert_eq!(DeviceLost.indicator_color(), "rgb(245, 190, 80)");

        // Test indicator pulse
        assert!(AutoReconnecting.indicator_should_pulse());
        assert!(!Connected.indicator_should_pulse());
        assert!(!Disconnected.indicator_should_pulse());

        // Test disconnect button
        assert!(Connected.can_disconnect());
        assert!(DeviceLost.can_disconnect());
        assert!(AutoReconnecting.can_disconnect());
        assert!(!Connecting.can_disconnect());
        assert!(!Disconnected.can_disconnect());

        // Test status text
        assert_eq!(Connected.status_text(), "Connected");
        assert_eq!(Disconnected.status_text(), "Ready to connect");
        assert_eq!(Probing.status_text(), "Detecting firmware...");
    }

    #[test]
    fn test_idempotent_transitions() {
        use ConnectionState::*;

        // Some states allow idempotent transitions
        assert!(Disconnected.can_transition_to(Disconnected));
        assert!(AutoReconnecting.can_transition_to(AutoReconnecting));
    }

    #[test]
    fn test_disconnecting_is_one_way() {
        use ConnectionState::*;

        // Once in Disconnecting state, can only go to Disconnected
        assert!(Disconnecting.can_transition_to(Disconnected));
        assert!(!Disconnecting.can_transition_to(Connected));
        assert!(!Disconnecting.can_transition_to(Connecting));
        assert!(!Disconnecting.can_transition_to(DeviceLost));
    }

    #[test]
    fn test_usb_unplug_transitions() {
        use ConnectionState::*;

        // USB unplug can happen from most states
        assert!(Connected.can_transition_to(DeviceLost));
        assert!(Connected.can_transition_to(AutoReconnecting));
        assert!(Connecting.can_transition_to(DeviceLost));
        assert!(Reconfiguring.can_transition_to(DeviceLost));
    }

    #[test]
    fn test_all_states_covered() {
        use ConnectionState::*;

        // Ensure all 8 states are tested
        let all_states = vec![
            Disconnected,
            Probing,
            Connecting,
            Connected,
            Reconfiguring,
            Disconnecting,
            DeviceLost,
            AutoReconnecting,
        ];

        assert_eq!(all_states.len(), 8);

        // Test each state has at least one valid transition
        for state in all_states {
            let has_valid_transition = vec![
                Disconnected,
                Probing,
                Connecting,
                Connected,
                Reconfiguring,
                Disconnecting,
                DeviceLost,
                AutoReconnecting,
            ]
            .iter()
            .any(|&to| state.can_transition_to(to));

            assert!(
                has_valid_transition,
                "{:?} should have at least one valid transition",
                state
            );
        }
    }

    #[test]
    fn test_states_visually_distinguishable() {
        use ConnectionState::*;

        // Each state should have unique visual representation
        let states = vec![
            Disconnected,
            Probing,
            Connecting,
            Connected,
            Reconfiguring,
            Disconnecting,
            DeviceLost,
            AutoReconnecting,
        ];

        for (i, &s1) in states.iter().enumerate() {
            for &s2 in states.iter().skip(i + 1) {
                // States should differ in color OR pulse OR text
                let differs = s1.indicator_color() != s2.indicator_color()
                    || s1.indicator_should_pulse() != s2.indicator_should_pulse()
                    || s1.status_text() != s2.status_text();

                assert!(
                    differs,
                    "{:?} and {:?} should be visually distinguishable",
                    s1, s2
                );
            }
        }
    }

    /// Test parse_framing with different stop bits
    #[test]
    fn test_parse_framing_stop_bits() {
        // 1 stop bit (most common)
        let (_, _, s) = parse_framing("8N1");
        assert_eq!(s, 1);

        // 2 stop bits
        let (_, _, s) = parse_framing("8N2");
        assert_eq!(s, 2);

        // parse_framing parses any digit directly (no validation)
        // So "8N3" will parse as 3, even though it's not a standard stop bit value
        let (_, _, s) = parse_framing("8N3");
        assert_eq!(s, 3); // Parsed as-is (no validation in parse_framing)

        // Invalid stop bit character defaults to 1
        let (_, _, s) = parse_framing("8NX");
        assert_eq!(s, 1); // Non-digit defaults to 1
    }

    // ============ State Transition Sequence Tests ============

    /// Test normal connection flow sequence
    #[test]
    fn test_connection_flow_manual() {
        use ConnectionState::*;

        // Start: Disconnected
        let state = Disconnected;
        assert!(state.can_transition_to(Connecting));

        // Connecting...
        let state = Connecting;
        assert!(state.can_transition_to(Connected));

        // Connected!
        let state = Connected;
        assert!(state.can_disconnect());
        assert!(state.can_transition_to(Disconnecting));

        // Disconnecting...
        let state = Disconnecting;
        assert!(!state.can_disconnect());
        assert!(state.can_transition_to(Disconnected));

        // Back to Disconnected
        let state = Disconnected;
        assert!(!state.can_disconnect());
    }

    /// Test auto-detection flow sequence
    #[test]
    fn test_connection_flow_auto_detect() {
        use ConnectionState::*;

        // Start: Disconnected
        let state = Disconnected;
        assert!(state.can_transition_to(Probing));

        // Probing for firmware...
        let state = Probing;
        assert!(!state.can_disconnect()); // Cannot disconnect while probing
        assert!(state.can_transition_to(Connecting));

        // Found firmware, connecting...
        let state = Connecting;
        assert!(state.can_transition_to(Connected));

        // Connected!
        let _state = Connected;
    }

    /// Test device loss and recovery flow
    #[test]
    fn test_device_loss_flow() {
        use ConnectionState::*;

        // Start: Connected
        let state = Connected;

        // USB unplugged (no VID/PID for auto-reconnect)
        assert!(state.can_transition_to(DeviceLost));
        let state = DeviceLost;

        // User can give up
        assert!(state.can_disconnect());
        assert!(state.can_transition_to(Disconnected));
    }

    /// Regression test: DeviceLost → Disconnecting should be allowed
    /// Bug: User unplugs USB, then clicks disconnect button, causing panic
    #[test]
    fn test_bug_device_lost_disconnect() {
        use ConnectionState::*;

        // Bug scenario: USB unplugged while connected
        let state = DeviceLost;

        // User clicks disconnect button to give up and return to ready state
        // This should allow transition through Disconnecting state
        assert!(
            state.can_transition_to(Disconnecting),
            "DeviceLost MUST allow transition to Disconnecting (user giving up)"
        );

        // Also verify can_disconnect() returns true
        assert!(
            state.can_disconnect(),
            "DeviceLost should allow disconnect button"
        );

        // Complete flow: DeviceLost → Disconnecting → Disconnected
        let state = Disconnecting;
        assert!(state.can_transition_to(Disconnected));
    }

    /// Test auto-reconnect flow
    #[test]
    fn test_auto_reconnect_flow() {
        use ConnectionState::*;

        // Start: Connected
        let state = Connected;

        // USB unplugged (has VID/PID for auto-reconnect)
        assert!(state.can_transition_to(AutoReconnecting));
        let state = AutoReconnecting;

        // Retry loop
        assert!(state.can_transition_to(AutoReconnecting)); // Idempotent
        assert!(state.can_transition_to(DeviceLost)); // Retry failed
        assert!(state.can_transition_to(Connected)); // Retry succeeded

        // User can cancel
        assert!(state.can_disconnect());
        assert!(state.can_transition_to(Disconnected));
    }

    /// Test reconfiguration flow
    #[test]
    fn test_reconfiguration_flow() {
        use ConnectionState::*;

        // Start: Connected
        let state = Connected;
        assert!(state.can_transition_to(Reconfiguring));

        // Reconfiguring baud rate...
        let state = Reconfiguring;
        assert!(!state.can_disconnect()); // Cannot disconnect during reconfig
        assert!(state.can_transition_to(Connected)); // Success
        assert!(state.can_transition_to(DeviceLost)); // USB unplugged during reconfig
    }

    /// Test reconfigure() operation sequence
    /// Regression test for bug: reconfigure() called disconnect() after transitioning
    /// to Reconfiguring state, causing disconnect() to always fail.
    #[test]
    fn test_reconfigure_disconnect_sequence() {
        use ConnectionState::*;

        // BUG: If we transition to Reconfiguring BEFORE calling disconnect(),
        // then disconnect() will fail because can_disconnect() returns false
        // for Reconfiguring state.
        //
        // CORRECT SEQUENCE:
        // 1. Start in Connected state
        let state = Connected;
        assert!(state.can_disconnect()); // Must be true for disconnect() to work

        // 2. Call disconnect() while STILL in Connected state
        //    (disconnect() will transition to Disconnected)

        // 3. THEN transition to Reconfiguring state
        //    (this prevents user from canceling mid-reconfiguration)

        // 4. Reconnect with new parameters
        let state = Reconfiguring;
        assert!(state.can_transition_to(Connected));

        // VERIFY: The bug would manifest as:
        // Connected -> Reconfiguring -> disconnect() fails -> stuck in Reconfiguring
        assert!(
            !Reconfiguring.can_disconnect(),
            "Reconfiguring state should not allow disconnect to prevent user cancellation"
        );
    }

    /// Test probing interrupted by device re-enumeration
    #[test]
    fn test_probing_interrupt_recovery_flow() {
        use ConnectionState::*;

        // Start: Probing
        let state = Probing;

        // Device disconnects during probe (OS closes port)
        // onconnect fires (device re-enumerated)
        assert!(state.can_transition_to(AutoReconnecting));
        let state = AutoReconnecting;

        // Successfully reconnect
        assert!(state.can_transition_to(Connected));
    }

    // ============ Fail-Fast Validation Tests ============

    /// Test that invalid state combinations are impossible
    /// These should be caught at compile time or test time, not runtime
    #[test]
    fn test_no_invalid_state_combinations() {
        use ConnectionState::*;

        // Cannot be both connecting and disconnecting
        // (enforced by single state enum, not multiple booleans)

        // Cannot be connected while probing
        assert!(!Probing.can_transition_to(Connected));
        assert!(!Connected.can_transition_to(Probing));

        // Cannot be connected while disconnecting
        assert!(!Disconnecting.can_transition_to(Connected));
        assert!(!Connected
            .can_transition_to(Disconnecting)
            .then(|| Disconnecting.can_transition_to(Connected))
            .unwrap_or(true));
    }

    /// Test that all transition paths are deterministic
    #[test]
    fn test_deterministic_transitions() {
        use ConnectionState::*;

        // Each state has clear, deterministic exit paths
        // No ambiguity about what state to transition to

        // Disconnecting MUST go to Disconnected (no other option)
        assert!(Disconnecting.can_transition_to(Disconnected));
        assert!(!Disconnecting.can_transition_to(Connected));
        assert!(!Disconnecting.can_transition_to(Connecting));

        // Connecting can only go to Connected or back to Disconnected
        // (or DeviceLost if USB unplugged)
        let valid_connecting_exits = [Connected, Disconnected, DeviceLost];
        for state in [
            Probing,
            Connecting,
            Reconfiguring,
            Disconnecting,
            AutoReconnecting,
        ] {
            if !valid_connecting_exits.contains(&state) {
                assert!(!Connecting.can_transition_to(state));
            }
        }
    }
}
