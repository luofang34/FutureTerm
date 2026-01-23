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
            (Disconnected, DeviceLost) => true, // ondisconnect fires after cleanup
            (Disconnected, AutoReconnecting) => true, // auto-reconnect fires after cleanup

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

/// RAII guard that ensures atomic state is unlocked and signal is synced
///
/// This guard is returned by `begin_exclusive_transition()` and ensures that:
/// 1. The atomic state is always unlocked (either explicitly via `finish()` or on Drop)
/// 2. The signal is always synchronized with the atomic state
/// 3. Both atomic and signal are updated together atomically
///
/// # Usage
/// ```
/// let guard = manager.begin_exclusive_transition(current, Connecting)?;
/// // ... do work ...
/// guard.finish(Connected); // Success: unlock to Connected + sync signal
/// // Or: drop(guard); // Error: auto-unlock to Disconnected + sync signal
/// ```
pub struct ExclusiveTransitionGuard {
    pub(crate) atomic_state: Arc<AtomicConnectionState>,
    pub(crate) set_state: WriteSignal<ConnectionState>,
    pub(crate) set_status: WriteSignal<String>,
}

impl ExclusiveTransitionGuard {
    /// Finish transition successfully (unlock + sync signal)
    ///
    /// This unlocks the atomic state to the specified final state and updates
    /// the signal to match. The guard is consumed and Drop will not run.
    pub fn finish(self, final_state: ConnectionState) {
        // Unlock atomic
        self.atomic_state.unlock_and_set(final_state);

        // Update signal to match
        self.set_state.set(final_state);
        self.set_status.set(final_state.status_text().to_string());

        // Don't run Drop (we handled cleanup)
        std::mem::forget(self);
    }
}

impl Drop for ExclusiveTransitionGuard {
    fn drop(&mut self) {
        // Panic/error path: unlock to Disconnected and sync signal
        // This ensures BOTH atomic and signal are always synchronized
        self.atomic_state
            .unlock_and_set(ConnectionState::Disconnected);
        self.set_state.set(ConnectionState::Disconnected);
        self.set_status.set("Ready to connect".to_string());

        #[cfg(debug_assertions)]
        web_sys::console::warn_1(
            &"ExclusiveTransitionGuard dropped without finish() - unlocking to Disconnected".into(),
        );
    }
}

/// RAII guard that only manages the lock bit (for orchestrator methods)
///
/// This simpler guard is for orchestrator methods like `connect()` that need to
/// prevent concurrent operations but delegate state management to sub-methods.
///
/// Unlike ExclusiveTransitionGuard, this guard:
/// - Does NOT set state when acquired (sub-methods call transition_to())
/// - Only clears the lock bit on Drop (does NOT change state to Disconnected)
///
/// # Usage
/// ```
/// let _lock = manager.acquire_operation_lock()?;
/// // Sub-methods can call transition_to() freely
/// manager.connect_with_auto_detect(port, framing).await?;
/// // Lock automatically released on Drop (state unchanged)
/// ```
pub struct OperationLockGuard {
    pub(crate) atomic_state: Arc<AtomicConnectionState>,
}

impl ConnectionManager {
    /// Securely finalize a connection state transition (update both signal and atomic)
    /// This helper prevents desynchronization bugs where signal is updated but atomic isn't.
    /// It preserves the atomic lock bit if present.
    pub fn finalize_connection(&self, state: ConnectionState) {
        self.atomic_state.set_preserving_lock(state);
        self.set_state.set(state);
    }
}

impl Drop for OperationLockGuard {
    fn drop(&mut self) {
        // Just clear the lock bit, don't change state
        // Sub-methods have already set the correct state via transition_to()
        self.atomic_state.unlock();

        #[cfg(debug_assertions)]
        web_sys::console::log_1(&"OperationLockGuard dropped - unlocking".into());
    }
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

    /// Try to acquire lock without changing state
    ///
    /// This is for orchestrator methods (like connect()) that need to prevent
    /// concurrent operations but delegate state management to sub-methods.
    ///
    /// Returns Ok(current_state) if lock acquired, Err(current_state) if already locked.
    pub fn try_lock(&self) -> Result<ConnectionState, ConnectionState> {
        let current = self.state.load(Ordering::SeqCst);

        // If already locked, fail
        if (current & LOCK_FLAG) != 0 {
            let state = ConnectionState::from_u8(current & STATE_MASK)
                .unwrap_or(ConnectionState::Disconnected);
            return Err(state);
        }

        // Try to set lock bit without changing state
        let locked = current | LOCK_FLAG;
        match self
            .state
            .compare_exchange(current, locked, Ordering::SeqCst, Ordering::SeqCst)
        {
            Ok(_) => {
                let state = ConnectionState::from_u8(current & STATE_MASK)
                    .unwrap_or(ConnectionState::Disconnected);
                Ok(state)
            }
            Err(actual) => {
                let state = ConnectionState::from_u8(actual & STATE_MASK)
                    .unwrap_or(ConnectionState::Disconnected);
                Err(state)
            }
        }
    }

    /// Unlock without changing state
    /// Must only be called by the task that holds the lock.
    pub fn unlock(&self) {
        let current = self.state.load(Ordering::SeqCst);
        let unlocked = current & STATE_MASK; // Clear lock bit, keep state
        self.state.store(unlocked, Ordering::SeqCst);
    }

    /// Set state value while preserving lock status
    /// This is used by transition_to() to update state during locked operations
    pub fn set_preserving_lock(&self, new_state: ConnectionState) {
        let current = self.state.load(Ordering::SeqCst);
        let is_locked = (current & LOCK_FLAG) != 0;

        let new_value = if is_locked {
            new_state.to_u8() | LOCK_FLAG
        } else {
            new_state.to_u8()
        };

        self.state.store(new_value, Ordering::SeqCst);
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

    // ============ Synchronization and Guard Tests ============

    /// Test that try_transition enforces single-lock semantics
    /// Critical: Only one transition can hold the lock at a time
    #[test]
    fn test_synchronization_single_lock() {
        let state = Arc::new(AtomicConnectionState::new(ConnectionState::Disconnected));

        // First transition acquires lock
        let r1 = state.try_transition(ConnectionState::Disconnected, ConnectionState::Connecting);
        assert!(r1.is_ok(), "First transition should succeed");
        assert_eq!(state.get(), ConnectionState::Connecting);
        assert!(state.is_locked(), "State should be locked");

        // Second transition fails (still locked)
        let r2 = state.try_transition(ConnectionState::Connecting, ConnectionState::Connected);
        assert!(r2.is_err(), "Second transition should fail while locked");
        assert_eq!(
            state.get(),
            ConnectionState::Connecting,
            "State should not change"
        );

        // After unlock, new transition succeeds
        state.unlock_and_set(ConnectionState::Connected);
        assert!(!state.is_locked(), "State should be unlocked");

        let r3 = state.try_transition(ConnectionState::Connected, ConnectionState::Disconnecting);
        assert!(r3.is_ok(), "Transition after unlock should succeed");
        assert!(state.is_locked(), "State should be locked again");
    }

    /// Test that unlock_and_set clears lock bit correctly
    #[test]
    fn test_synchronization_unlock_clears_lock() {
        let state = AtomicConnectionState::new(ConnectionState::Disconnected);

        // Lock by transitioning
        state
            .try_transition(ConnectionState::Disconnected, ConnectionState::Connecting)
            .unwrap();
        assert!(state.is_locked());

        // Unlock to different state
        state.unlock_and_set(ConnectionState::Connected);

        // Verify: state changed AND lock cleared
        assert_eq!(state.get(), ConnectionState::Connected);
        assert!(
            !state.is_locked(),
            "Lock should be cleared after unlock_and_set"
        );

        // Verify: can transition again (not locked)
        let result =
            state.try_transition(ConnectionState::Connected, ConnectionState::Disconnecting);
        assert!(result.is_ok(), "Should be able to transition after unlock");
    }

    /// Test that try_transition validates state machine rules
    /// Critical: Prevents invalid transitions at atomic level
    #[test]
    fn test_synchronization_validates_transitions() {
        use ConnectionState::*;

        let invalid_transitions = vec![
            (Disconnected, Connected),  // Must go through Connecting
            (Connected, Disconnected),  // Must go through Disconnecting
            (Disconnecting, Connected), // Disconnecting is one-way
            (Probing, Disconnecting),   // Cannot disconnect while probing
        ];

        for (from, to) in invalid_transitions {
            let state = AtomicConnectionState::new(from);
            let result = state.try_transition(from, to);

            assert!(
                result.is_err(),
                "try_transition should reject invalid transition {:?} → {:?}",
                from,
                to
            );
            assert_eq!(
                state.get(),
                from,
                "State should not change on invalid transition"
            );
            assert!(
                !state.is_locked(),
                "State should not be locked after failed transition"
            );
        }
    }

    /// Test concurrent access patterns
    /// Verifies that multiple tasks cannot acquire lock simultaneously
    #[test]
    fn test_synchronization_concurrent_access() {
        let state = Arc::new(AtomicConnectionState::new(ConnectionState::Disconnected));

        // Task 1: Acquires lock for Connecting
        let r1 = state.try_transition(ConnectionState::Disconnected, ConnectionState::Connecting);
        assert!(r1.is_ok());

        // Task 2: Tries to acquire lock for different transition
        let r2 = state.try_transition(ConnectionState::Disconnected, ConnectionState::Probing);
        assert!(r2.is_err(), "Concurrent transition should fail");

        // Task 3: Tries to transition from current state (still locked)
        let r3 = state.try_transition(ConnectionState::Connecting, ConnectionState::Connected);
        assert!(r3.is_err(), "Cannot transition while locked");

        // Task 1 completes: unlocks
        state.unlock_and_set(ConnectionState::Connected);

        // Task 2 retries: succeeds now
        let r4 = state.try_transition(ConnectionState::Connected, ConnectionState::Disconnecting);
        assert!(r4.is_ok(), "Should succeed after unlock");
    }

    /// Test lock/unlock cycle integrity
    /// Verifies that lock bit is managed correctly through full cycle
    #[test]
    fn test_synchronization_lock_cycle_integrity() {
        let state = AtomicConnectionState::new(ConnectionState::Disconnected);

        // Cycle: Disconnected → Connecting (locked) → Connected (unlocked)
        //                     → Disconnecting (locked) → Disconnected (unlocked)

        // Phase 1: Lock for Connecting
        assert!(!state.is_locked());
        state
            .try_transition(ConnectionState::Disconnected, ConnectionState::Connecting)
            .unwrap();
        assert!(state.is_locked());
        assert_eq!(state.get(), ConnectionState::Connecting);

        // Phase 2: Unlock to Connected
        state.unlock_and_set(ConnectionState::Connected);
        assert!(!state.is_locked());
        assert_eq!(state.get(), ConnectionState::Connected);

        // Phase 3: Lock for Disconnecting
        state
            .try_transition(ConnectionState::Connected, ConnectionState::Disconnecting)
            .unwrap();
        assert!(state.is_locked());
        assert_eq!(state.get(), ConnectionState::Disconnecting);

        // Phase 4: Unlock to Disconnected
        state.unlock_and_set(ConnectionState::Disconnected);
        assert!(!state.is_locked());
        assert_eq!(state.get(), ConnectionState::Disconnected);

        // Verify: Can start new cycle
        let result =
            state.try_transition(ConnectionState::Disconnected, ConnectionState::Connecting);
        assert!(result.is_ok(), "Should be able to start new cycle");
    }

    /// Test that get() always returns current state regardless of lock
    /// Lock bit should be transparent to state queries
    #[test]
    fn test_synchronization_get_ignores_lock() {
        let state = AtomicConnectionState::new(ConnectionState::Disconnected);

        // Unlocked: get() returns Disconnected
        assert_eq!(state.get(), ConnectionState::Disconnected);
        assert!(!state.is_locked());

        // Locked: get() still returns current state (ignores lock bit)
        state
            .try_transition(ConnectionState::Disconnected, ConnectionState::Connecting)
            .unwrap();
        assert_eq!(
            state.get(),
            ConnectionState::Connecting,
            "get() should return state regardless of lock"
        );
        assert!(state.is_locked());

        // Verify lock bit doesn't affect state value
        let state_value = state.get();
        assert!(matches!(state_value, ConnectionState::Connecting));
    }

    // Safety Test against regression
    #[test]
    fn test_finalize_connection_synchronizes_states() {
        // Mock logic: Verify set_preserving_lock works as intended for finalize_connection
        let atomic = AtomicConnectionState::new(ConnectionState::Connecting);

        // 1. Simulate locked operation (like connect_impl)
        atomic.try_lock().unwrap();
        assert!(atomic.is_locked());
        assert_eq!(atomic.get(), ConnectionState::Connecting);

        // 2. "Finalize" connection (transition to Connected while locked)
        atomic.set_preserving_lock(ConnectionState::Connected);

        // 3. Verify INVARIANTS:
        assert_eq!(
            atomic.get(),
            ConnectionState::Connected,
            "Atomic state must update"
        );
        assert!(
            atomic.is_locked(),
            "Lock must be preserved (for OperationLockGuard to unlock)"
        );
    }
}
