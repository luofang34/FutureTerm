use leptos::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

use core_types::{SerialConfig, Transport};
use transport_webserial::WebSerialTransport;
use wasm_bindgen_futures::spawn_local;
use web_sys::Worker;

use futures::select;
use futures::stream::StreamExt;
use futures::FutureExt;
use futures_channel::{mpsc, oneshot};

// We need to move the protocol module usage here or make it public
use crate::protocol::UiToWorker;

// ============ Logging Macros ============

/// Debug logging - can be disabled in release builds for performance
/// In debug builds, logs to console. In release, compiles to no-op.
#[cfg(debug_assertions)]
macro_rules! debug_log {
    ($($arg:tt)*) => {
        web_sys::console::log_1(&format!($($arg)*).into())
    };
}

#[cfg(not(debug_assertions))]
macro_rules! debug_log {
    ($($arg:tt)*) => {
        // No-op in release builds
    };
}

/// Warning logging - always enabled
macro_rules! warn_log {
    ($($arg:tt)*) => {
        web_sys::console::warn_1(&format!($($arg)*).into())
    };
}

/// Error logging - always enabled
macro_rules! error_log {
    ($($arg:tt)*) => {
        web_sys::console::error_1(&format!($($arg)*).into())
    };
}

// ============ Timing Constants ============

/// Adaptive polling intervals for device re-enumeration
const POLL_INTERVAL_FAST_MS: i32 = 100; // First 5 attempts (0-500ms)
const POLL_INTERVAL_MEDIUM_MS: i32 = 200; // Next 5 attempts (500-1500ms)
const POLL_INTERVAL_SLOW_MS: i32 = 400; // Final 5 attempts (1500-3500ms)

/// Wait time for device to be ready after re-enumeration
const REENUMERATION_WAIT_MS: i32 = 200;

/// Wait time for OS to release port after probing
const PROBING_CLEANUP_WAIT_MS: i32 = 150;

/// Auto-reconnect retry configuration
const AUTO_RECONNECT_RETRY_INTERVAL_MS: i32 = 50;
const AUTO_RECONNECT_MAX_RETRIES: usize = 200; // 200 * 50ms = 10 seconds max

/// Timeout for read loop completion during disconnect
const DISCONNECT_COMPLETION_TIMEOUT_MS: i32 = 2000; // 2 seconds

// ============ Connection State Machine ============

/// # Connection State Machine
///
/// This module implements a unified state machine for managing serial port connections.
/// The state machine prevents race conditions, invalid state combinations, and provides
/// a single source of truth for connection status.
///
/// ## State Transition Diagram
///
/// ```text
///                         ┌─────────────────────────────┐
///                         │      Disconnected           │◄─────────────┐
///                         │   (Ready to connect)        │              │
///                         └──────────┬──────────────────┘              │
///                                    │                                  │
///                      ┌─────────────┼─────────────┐                   │
///                      │             │             │                   │
///                      │ (auto)      │ (manual)    │                   │
///                      ▼             ▼             │                   │
///           ┌──────────────┐  ┌─────────────┐     │                   │
///           │   Probing    │  │ Connecting  │     │                   │
///           │ (detect FW)  │  │             │     │                   │
///           └──┬───────┬───┘  └──┬──────┬───┘     │                   │
///              │       │         │      │         │                   │
///         (ok) │       │ (fail)  │      │ (fail)  │                   │
///              ▼       └─────────┼──────┘         │                   │
///           ┌─────────────┐      │                │                   │
///           │ Connecting  │      │                │                   │
///           └──────┬──────┘      │                │                   │
///                  │             │                │                   │
///             (ok) │             │                │                   │
///                  ▼             │                │                   │
///           ┌─────────────────┐  │                │                   │
///           │    Connected    │◄─┘                │                   │
///           │                 │                   │                   │
///           └──┬────┬───┬──┬──┘                   │                   │
///              │    │   │  │                      │                   │
///    (reconfig)│    │   │  │(USB unplug)          │                   │
///              │    │   │  └────────┐             │                   │
///              ▼    │   │           ▼             │                   │
///      ┌──────────┐ │   │  ┌────────────────┐    │                   │
///      │Reconfig  │ │   │  │   DeviceLost   │    │                   │
///      │          │ │   │  │ (waiting...)   │    │                   │
///      └─┬────┬───┘ │   │  └───┬────────┬───┘    │                   │
///        │(ok)│(USB)│   │      │        │        │                   │
///        │    │     │   │      │(USB)   │(cancel)│                   │
///        ▼    │     │   │      ▼        └────────┼───────────────────┘
///    Connected◄─────┘   │  ┌──────────────────┐  │
///                       │  │AutoReconnecting  │  │
///        (user disc)    │  │   (retry loop)   │  │
///                       │  └──┬───────────┬───┘  │
///                       │     │           │      │
///                       │(ok) │           │(fail)│
///                       │     │           │      │
///                       │     └───────────┼──────┘
///                       │                 │
///                       ▼                 │
///              ┌──────────────┐           │
///              │Disconnecting │           │
///              └──────┬───────┘           │
///                     │                   │
///                (ok) │                   │
///                     └───────────────────┘
/// ```
///
/// ## Valid State Transitions
///
/// | From State       | To State         | Trigger                           |
/// |------------------|------------------|-----------------------------------|
/// | Disconnected     | Probing          | User clicks connect (auto-detect) |
/// | Disconnected     | Connecting       | User clicks connect (manual)      |
/// | Probing          | Connecting       | Firmware detected                 |
/// | Probing          | Disconnected     | Probe failed/canceled             |
/// | Connecting       | Connected        | Connection established            |
/// | Connecting       | Disconnected     | Connection failed                 |
/// | Connecting       | DeviceLost       | USB unplugged during connect      |
/// | Connected        | Reconfiguring    | Baud rate changed                 |
/// | Connected        | Disconnecting    | User clicks disconnect            |
/// | Connected        | DeviceLost       | USB unplugged (no auto-reconnect) |
/// | Connected        | AutoReconnecting | USB unplugged (has VID/PID)       |
/// | Reconfiguring    | Connected        | Reconfiguration complete          |
/// | Reconfiguring    | DeviceLost       | USB unplugged during reconfig     |
/// | Disconnecting    | Disconnected     | Disconnect complete               |
/// | DeviceLost       | Disconnected     | User cancels (clicks disconnect)  |
/// | DeviceLost       | AutoReconnecting | Device replugged (VID/PID match)  |
/// | DeviceLost       | Connecting       | Manual reconnect attempt          |
/// | AutoReconnecting | Connected        | Reconnection successful           |
/// | AutoReconnecting | DeviceLost       | Reconnection failed (retry)       |
/// | AutoReconnecting | Disconnected     | User cancels auto-reconnect       |
///
/// ## Usage Examples
///
/// ### Basic State Queries
/// ```rust
/// let state = manager.state.get();
///
/// // Check if user can disconnect
/// if state.can_disconnect() {
///     // Show "Disconnect" button
/// }
///
/// // Get indicator color and animation
/// let color = state.indicator_color();  // "rgb(95, 200, 85)" for Connected
/// let should_pulse = state.indicator_should_pulse();  // true for AutoReconnecting
/// ```
///
/// ### State Transitions
/// ```rust
/// // In connection code - use transition_to() which validates transitions
/// self.transition_to(ConnectionState::Connecting);  // Validates and logs transition
/// // ... do connection work ...
/// self.transition_to(ConnectionState::Connected);
///
/// // Invalid transition will log error but not panic in release
/// // This allows graceful degradation while catching bugs in development
/// ```
///
/// ### Event Handlers
/// ```rust
/// // In USB disconnect event handler
/// let current_state = self.state.get_untracked();
///
/// if matches!(current_state, ConnectionState::Connected) {
///     // Determine if we should auto-reconnect
///     if has_vid_pid {
///         self.transition_to(ConnectionState::AutoReconnecting);
///     } else {
///         self.transition_to(ConnectionState::DeviceLost);
///     }
/// }
/// ```
///
/// ## Design Rationale
///
/// ### Why State Machine Over Boolean Flags?
///
/// **Before (Boolean Flags):**
/// - `connected: bool`
/// - `is_auto_reconnecting: bool`
/// - `is_connecting: bool`
/// - `is_disconnecting: bool`
/// - `is_reconfiguring: bool`
/// - `is_probing: bool`
///
/// **Problems:**
/// - 2^6 = 64 possible combinations, most invalid
/// - Race conditions when flags update asynchronously
/// - No single source of truth
/// - Hard to determine "what can I do now?"
///
/// **After (State Machine):**
/// - Single `ConnectionState` enum
/// - Only 8 valid states (not 64)
/// - Atomic transitions
/// - Type-safe: compiler prevents invalid states
/// - Clear intent: `state.can_disconnect()` vs checking 4 flags
///
/// ### Runtime vs Compile-Time Validation
///
/// The `can_transition_to()` method provides runtime validation that:
/// 1. **In debug builds**: Logs errors and fails tests (helps catch bugs early)
/// 2. **In release builds**: Logs errors but allows graceful degradation
/// 3. **At compile time**: Exhaustive match ensures all transitions are considered
///
/// This hybrid approach gives us:
/// - Development-time safety (tests catch invalid transitions)
/// - Production robustness (no panics from unexpected states)
/// - Maintainability (compiler warns if new states added without updating transitions)
///
/// Unified connection state for robust state management
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
            (DeviceLost, AutoReconnecting) => true, // Device replugged
            (DeviceLost, Connecting) => true,   // Manual reconnect attempt

            // From AutoReconnecting
            (AutoReconnecting, Connected) => true, // Reconnect successful
            (AutoReconnecting, DeviceLost) => true, // Reconnect failed
            (AutoReconnecting, Disconnected) => true, // User cancels
            (AutoReconnecting, AutoReconnecting) => true, // Idempotent retry

            // All other transitions are invalid
            _ => false,
        }
    }
}

// Message passing infrastructure
enum ConnectionCommand {
    Stop,
    Write {
        data: Vec<u8>,
        response: oneshot::Sender<Result<(), String>>,
    },
}

struct ConnectionHandle {
    cmd_tx: mpsc::UnboundedSender<ConnectionCommand>,
    completion_rx: oneshot::Receiver<()>,
}

// Type alias for event handler closures to avoid clippy::type_complexity warning
type EventClosures = (
    Closure<dyn FnMut(web_sys::Event)>,
    Closure<dyn FnMut(web_sys::Event)>,
);

// Snapshot for UI state consistency (not true transaction rollback)
// WebSerial API doesn't support true rollback since port can't be opened twice
// This captures UI-visible state for restoration on reconfigure failure
#[derive(Clone)]
struct ConnectionSnapshot {
    baud: u32,
    framing: String,
    decoder_id: String, /* Protocol decoder (e.g., "utf8", "mavlink")
                         * Note: VID/PID not captured - they don't change during reconfiguration
                         * Note: Connection handle not captured - cannot be restored (port must
                         * be reopened) */
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
    set_decoder_id: WriteSignal<String>, // Private - use set_decoder() method

    // ============ Activity Indicators (transient pulses) ============
    // Note: WriteSignals are private - use trigger_rx()/trigger_tx() methods
    pub rx_active: Signal<bool>,
    set_rx_active: WriteSignal<bool>,
    pub tx_active: Signal<bool>,
    set_tx_active: WriteSignal<bool>,

    // ============ Internal State (private) ============
    connection_handle: Rc<RefCell<Option<ConnectionHandle>>>,
    active_port: Rc<RefCell<Option<web_sys::SerialPort>>>,
    worker: Signal<Option<Worker>>, // Read-only access to worker for sending data
    last_auto_baud: Rc<RefCell<Option<u32>>>,
    // Atomic guards for connection state
    is_connecting: Arc<AtomicBool>,
    is_disconnecting: Arc<AtomicBool>,

    /// User-initiated disconnect flag (to distinguish from device loss)
    ///
    /// Using `Cell<bool>` instead of `RefCell<bool>` for consistency with atomics.
    /// Cell provides interior mutability without runtime borrowing checks (simpler).
    /// Safe in WASM single-threaded environment.
    user_initiated_disconnect: Rc<Cell<bool>>,

    // Auto-reconnect device tracking (to clear on user disconnect)
    set_last_vid: Rc<RefCell<Option<WriteSignal<Option<u16>>>>>,
    set_last_pid: Rc<RefCell<Option<WriteSignal<Option<u16>>>>>,

    /// Track if probing was interrupted by ondisconnect events
    ///
    /// **Purpose**: Prevent unnecessary polling delays when device re-enumeration is detected
    ///
    /// **Problem**: During auto-detection (probing), the OS may close the port causing an
    /// `ondisconnect` event. We then poll for the device to return. But if the device
    /// re-enumerates quickly, we'll get an `onconnect` event before polling starts.
    ///
    /// **Solution**: Set this flag when `ondisconnect` fires during probing. The polling
    /// code checks this flag and if an `onconnect` already happened, it skips polling
    /// entirely and proceeds immediately.
    ///
    /// **Example Timeline**:
    /// ```text
    /// T=0ms:  Start probing port A
    /// T=50ms: OS closes port → ondisconnect event → set probing_interrupted=true
    /// T=60ms: Device re-enumerates → onconnect event
    /// T=70ms: Probing code checks probing_interrupted
    ///         → sees flag is set AND state is AutoReconnecting
    ///         → skips 100-400ms polling, proceeds immediately
    /// ```
    ///
    /// **Reset**: Cleared at start of each auto-detection attempt (when baud == 0)
    ///
    /// **Related**: See `handle_probing_with_interrupt_recovery()` and
    /// `poll_for_device_reenumeration()`
    ///
    /// Using `Cell<bool>` for consistency with other boolean flags.
    probing_interrupted: Rc<Cell<bool>>,

    // Event handler closures (to prevent memory leaks on re-initialization)
    event_closures: Rc<RefCell<Option<EventClosures>>>,
}

impl ConnectionManager {
    pub fn new(worker_signal: Signal<Option<Worker>>) -> Self {
        // Initialize state machine with Disconnected state
        let (state, set_state) = create_signal(ConnectionState::Disconnected);

        let (status, set_status) = create_signal("Ready to connect".to_string());
        let (detected_baud, set_detected_baud) = create_signal(0);
        let (detected_framing, set_detected_framing) = create_signal("".to_string());

        let (rx_active, set_rx_active) = create_signal(false);
        let (tx_active, set_tx_active) = create_signal(false);
        let (decoder_id, set_decoder_id) = create_signal("utf8".to_string());

        Self {
            state: state.into(),
            set_state,
            status: status.into(),
            set_status,
            detected_baud: detected_baud.into(),
            set_detected_baud,
            detected_framing: detected_framing.into(),
            set_detected_framing,
            connection_handle: Rc::new(RefCell::new(None)),
            active_port: Rc::new(RefCell::new(None)),
            worker: worker_signal,
            last_auto_baud: Rc::new(RefCell::new(None)),
            is_connecting: Arc::new(AtomicBool::new(false)),
            is_disconnecting: Arc::new(AtomicBool::new(false)),

            rx_active: rx_active.into(),
            set_rx_active,
            tx_active: tx_active.into(),
            set_tx_active,
            decoder_id: decoder_id.into(),
            set_decoder_id,
            user_initiated_disconnect: Rc::new(Cell::new(false)),
            set_last_vid: Rc::new(RefCell::new(None)),
            set_last_pid: Rc::new(RefCell::new(None)),
            probing_interrupted: Rc::new(Cell::new(false)),
            event_closures: Rc::new(RefCell::new(None)),
        }
    }

    pub fn trigger_rx(&self) {
        if !self.rx_active.get_untracked() {
            self.set_rx_active.set(true);
            let s = self.set_rx_active;
            // Blink for 100ms
            spawn_local(async move {
                let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(&mut |r, _| {
                    if let Some(window) = web_sys::window() {
                        let _ =
                            window.set_timeout_with_callback_and_timeout_and_arguments_0(&r, 100);
                    }
                }))
                .await;
                s.set(false);
            });
        }
    }

    pub fn trigger_tx(&self) {
        if !self.tx_active.get_untracked() {
            self.set_tx_active.set(true);
            let s = self.set_tx_active;
            // Blink for 70ms
            spawn_local(async move {
                let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(&mut |r, _| {
                    if let Some(window) = web_sys::window() {
                        let _ =
                            window.set_timeout_with_callback_and_timeout_and_arguments_0(&r, 70);
                    }
                }))
                .await;
                s.set(false);
            });
        }
    }

    /// Transition to a new connection state
    fn transition_to(&self, new_state: ConnectionState) {
        let old_state = self.state.get_untracked();

        // Validate state transition
        if !old_state.can_transition_to(new_state) {
            // ERROR: Invalid transition detected
            error_log!(
                "INVALID STATE TRANSITION: {:?} → {:?} (not allowed)",
                old_state,
                new_state
            );

            // COMPILE-TIME CHECK: In debug builds, this will panic and fail tests
            // In release builds, this is removed by the compiler (zero overhead)
            debug_assert!(
                old_state.can_transition_to(new_state),
                "Invalid state transition: {:?} → {:?}",
                old_state,
                new_state
            );

            // In release builds: Log error but allow transition (graceful degradation)
            // This prevents production crashes while still catching bugs in development
        }

        // Log state transition for debugging
        web_sys::console::log_1(
            &format!("State transition: {:?} → {:?}", old_state, new_state).into(),
        );

        // Update state machine
        self.set_state.set(new_state);

        // Update status string
        self.set_status.set(new_state.status_text().into());
    }

    pub fn get_status(&self) -> Signal<String> {
        self.status
    }

    /// Clear stored VID/PID to prevent auto-reconnect
    /// Centralized method to avoid logic duplication across disconnect paths
    fn clear_auto_reconnect_device(&self) {
        if let Some(set_vid) = *self.set_last_vid.borrow() {
            set_vid.set(None);
        }
        if let Some(set_pid) = *self.set_last_pid.borrow() {
            set_pid.set(None);
        }
        web_sys::console::log_1(&"DEBUG: Cleared VID/PID to prevent auto-reconnect".into());
    }

    /// Handle probing interruption and port re-enumeration
    /// Returns (port, baud, framing, buffer, proto) tuple for connection
    async fn handle_probing_with_interrupt_recovery(
        &self,
        port: web_sys::SerialPort,
        baud: u32,
        framing: String,
        buffer: Vec<u8>,
        proto: Option<String>,
    ) -> Result<
        (
            web_sys::SerialPort,
            u32,
            String,
            Option<Vec<u8>>,
            Option<String>,
        ),
        String,
    > {
        // Check if probing was interrupted by ondisconnect events
        let was_interrupted = self.probing_interrupted.get();
        if was_interrupted {
            warn_log!("Probing was interrupted by ondisconnect event - port reference is stale");
            self.probing_interrupted.set(false); // Clear flag

            // When ondisconnect fires during probing, port reference becomes invalid
            // Wait for browser to re-enumerate device and get fresh port reference
            match self.poll_for_device_reenumeration().await {
                Ok(fresh_port) => Ok((fresh_port, baud, framing, Some(buffer), proto)),
                Err(e) => {
                    self.set_status.set(format!("Connection Failed: {}", e));
                    Err(e)
                }
            }
        } else {
            // Wait for OS to fully release port after probing
            debug_log!(
                "Probing complete, waiting {}ms for port cleanup",
                PROBING_CLEANUP_WAIT_MS
            );
            let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(&mut |r, _| {
                if let Some(window) = web_sys::window() {
                    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                        &r,
                        PROBING_CLEANUP_WAIT_MS,
                    );
                }
            }))
            .await;

            // Check interrupt flag AGAIN after cleanup wait
            // ondisconnect event may fire DURING the cleanup wait
            if self.probing_interrupted.get() {
                warn_log!("Disconnect occurred during cleanup wait - port is stale");
                self.probing_interrupted.set(false);

                // Use helper method to poll for fresh port
                match self.poll_for_device_reenumeration().await {
                    Ok(fresh_port) => Ok((fresh_port, baud, framing, Some(buffer), proto)),
                    Err(e) => {
                        self.set_status.set(format!("Connection Failed: {}", e));
                        Err(e)
                    }
                }
            } else {
                // No interrupt - use original port
                Ok((port, baud, framing, Some(buffer), proto))
            }
        }
    }

    /// Connect with smart framing detection (baud specified, framing auto)
    async fn connect_with_smart_framing(
        &self,
        port: web_sys::SerialPort,
        baud: u32,
    ) -> Result<(), String> {
        debug_log!("Connecting with smart framing detection: baud={}", baud);
        self.transition_to(ConnectionState::Connecting);

        let (detect_framing, initial_buf, proto) =
            self.smart_probe_framing(port.clone(), baud).await;

        if let Some(p) = proto {
            self.set_decoder(p);
        }

        self.connect_impl(port, baud, &detect_framing, Some(initial_buf))
            .await
    }

    /// Connect with full auto-detection (baud rate + framing)
    async fn connect_with_auto_detect(
        &self,
        port: web_sys::SerialPort,
        framing: &str,
    ) -> Result<(), String> {
        debug_log!("Connecting with auto-detection: framing={}", framing);
        self.transition_to(ConnectionState::Probing);

        let (baud, framing_str, buffer, proto) = self.detect_config(port.clone(), framing).await;

        // Handle potential interruption during probing
        let (final_port, final_baud, final_framing, initial_buffer, detected_proto) = self
            .handle_probing_with_interrupt_recovery(port, baud, framing_str, buffer, proto)
            .await?;

        // Switch decoder if detected
        if let Some(p) = detected_proto {
            self.set_decoder(p);
        }

        // Transition to Connecting before connect_impl()
        let current_state = self.state.get_untracked();
        if current_state == ConnectionState::Probing
            || current_state == ConnectionState::Disconnected
        {
            self.transition_to(ConnectionState::Connecting);
        }

        self.connect_impl(final_port, final_baud, &final_framing, initial_buffer)
            .await
    }

    /// Helper: Poll for device re-enumeration after disconnect events
    /// Uses adaptive polling intervals for optimal performance
    async fn poll_for_device_reenumeration(&self) -> Result<web_sys::SerialPort, String> {
        let Some(window) = web_sys::window() else {
            return Err("Window not available".to_string());
        };
        let nav = window.navigator();
        let serial = nav.serial();

        web_sys::console::log_1(
            &"DEBUG: Polling for device re-enumeration (max 3.5 seconds)".into(),
        );

        // OPTIMIZATION: Adaptive polling intervals
        // Devices typically re-enumerate within 500-1000ms, so use faster polling initially
        // First 5 attempts: 100ms (0-500ms)
        // Next 5 attempts: 200ms (500-1500ms)
        // Final 5 attempts: 400ms (1500-3500ms)
        let mut fresh_port: Option<web_sys::SerialPort> = None;
        let mut elapsed_ms = 0;

        for attempt in 1..=15 {
            match wasm_bindgen_futures::JsFuture::from(serial.get_ports()).await {
                Ok(ports_val) => {
                    let ports: js_sys::Array = ports_val.unchecked_into();
                    if ports.length() > 0 {
                        fresh_port = Some(ports.get(0).unchecked_into());
                        web_sys::console::log_1(
                            &format!(
                                "DEBUG: Device re-enumerated after {}ms (attempt {})",
                                elapsed_ms, attempt
                            )
                            .into(),
                        );
                        break;
                    }
                }
                Err(e) => {
                    web_sys::console::log_1(&format!("DEBUG: Failed to get ports: {:?}", e).into());
                    return Err("Cannot access ports".to_string());
                }
            }

            // Adaptive wait interval based on attempt number
            let wait_ms = if attempt <= 5 {
                POLL_INTERVAL_FAST_MS
            } else if attempt <= 10 {
                POLL_INTERVAL_MEDIUM_MS
            } else {
                POLL_INTERVAL_SLOW_MS
            };

            elapsed_ms += wait_ms;

            let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(&mut |r, _| {
                if let Some(window) = web_sys::window() {
                    let _ =
                        window.set_timeout_with_callback_and_timeout_and_arguments_0(&r, wait_ms);
                }
            }))
            .await;
        }

        if let Some(port) = fresh_port {
            // Wait a bit more for port to be fully ready
            web_sys::console::log_1(
                &format!(
                    "DEBUG: Waiting {}ms for port to be ready",
                    REENUMERATION_WAIT_MS
                )
                .into(),
            );
            let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(&mut |r, _| {
                if let Some(window) = web_sys::window() {
                    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                        &r,
                        REENUMERATION_WAIT_MS,
                    );
                }
            }))
            .await;
            Ok(port)
        } else {
            web_sys::console::log_1(&"DEBUG: Device not re-enumerated after 3 seconds".into());
            Err("Device not found after disconnect".to_string())
        }
    }

    /// Main connection entry point
    /// Handles three connection modes:
    /// 1. baud > 0, framing == "Auto" -> Smart framing detection
    /// 2. baud == 0 -> Full auto-detection (baud + framing)
    /// 3. baud > 0, framing != "Auto" -> Direct connection
    pub async fn connect(
        &self,
        port: web_sys::SerialPort,
        baud: u32,
        framing: &str,
    ) -> Result<(), String> {
        // Atomic test-and-set for connection guard
        if self
            .is_connecting
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            debug_log!("Connection attempt blocked: Already connecting");
            return Err("Already connecting".to_string());
        }

        // Ensure flag is cleared on ALL exit paths
        let _guard = scopeguard::guard((), |_| {
            self.is_connecting.store(false, Ordering::SeqCst);
        });

        // Stale UI Guard
        if self.active_port.borrow().is_some() {
            warn_log!("Connection attempt blocked: Port already active");
            return Err("Already connected".to_string());
        }

        // Reset probing interrupt flag if auto-detecting baud rate
        if baud == 0 {
            self.probing_interrupted.set(false);
        }

        // Route to appropriate connection method
        if baud > 0 && framing == "Auto" {
            // Mode 1: Smart framing detection with fixed baud
            self.connect_with_smart_framing(port, baud).await
        } else if baud == 0 {
            // Mode 2: Full auto-detection (baud + framing)
            self.connect_with_auto_detect(port, framing).await
        } else {
            // Mode 3: Direct connection with specified parameters
            debug_log!(
                "Connecting with fixed parameters: baud={}, framing={}",
                baud,
                framing
            );
            self.transition_to(ConnectionState::Connecting);
            self.connect_impl(port, baud, framing, None).await
        }
    }

    pub async fn disconnect(&self) -> bool {
        // Only allow disconnect from certain states
        let current_state = self.state.get_untracked();
        if !current_state.can_disconnect() {
            web_sys::console::log_1(
                &format!("Disconnect not allowed from state {:?}", current_state).into(),
            );
            return false;
        }

        // Atomic test-and-set for disconnect guard
        if self
            .is_disconnecting
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            web_sys::console::log_1(&"Disconnect already in progress".into());
            return false;
        }

        // Mark this as user-initiated disconnect
        self.user_initiated_disconnect.set(true);

        // Transition to appropriate state based on current state
        // DeviceLost/AutoReconnecting: Device already gone, skip to Disconnected
        // Connected: Transition through Disconnecting for proper cleanup
        let device_already_gone = matches!(
            current_state,
            ConnectionState::DeviceLost | ConnectionState::AutoReconnecting
        );

        if !device_already_gone {
            // Normal disconnect: transition through Disconnecting state
            self.transition_to(ConnectionState::Disconnecting);
        }

        // Send stop command to read loop and wait for completion
        let handle = self.connection_handle.borrow_mut().take();
        if let Some(h) = handle {
            web_sys::console::log_1(&"DEBUG: Sending Stop command to read loop".into());
            let _ = h.cmd_tx.unbounded_send(ConnectionCommand::Stop);

            // Wait for read loop to fully exit and drop transport (releases stream locks)
            // Use timeout to prevent indefinite blocking if read loop hangs
            web_sys::console::log_1(&"DEBUG: Waiting for read loop to release streams...".into());

            // Create timeout promise
            let timeout_promise = js_sys::Promise::new(&mut |resolve, _reject| {
                if let Some(window) = web_sys::window() {
                    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                        &resolve,
                        DISCONNECT_COMPLETION_TIMEOUT_MS,
                    );
                }
            });

            // Race completion against timeout
            let completion_future = h.completion_rx;
            let timeout_signal = wasm_bindgen_futures::JsFuture::from(timeout_promise);

            // Use select! to race futures
            futures::select! {
                result = completion_future.fuse() => {
                    match result {
                        Ok(_) => {
                            web_sys::console::log_1(
                                &"DEBUG: Read loop completed, streams released".into()
                            );
                        }
                        Err(_) => {
                            web_sys::console::log_1(
                                &"WARN: Read loop completion signal dropped (loop may have panicked)"
                                    .into(),
                            );
                        }
                    }
                }
                _ = timeout_signal.fuse() => {
                    warn_log!(
                        "Read loop did not complete within {}ms - proceeding with disconnect",
                        DISCONNECT_COMPLETION_TIMEOUT_MS
                    );
                }
            }
        }

        // Clear the port reference
        // Note: port.close() is already called by transport.close() in the read loop
        self.active_port.borrow_mut().take();

        // Clear VID/PID to prevent auto-reconnect after user-initiated disconnect
        // This is critical: when disconnect() is called from DeviceLost state, the device
        // is already physically gone, so ondisconnect event won't fire to clear VID/PID.
        // We must clear them here to prevent auto-reconnect when cable is replugged.
        self.clear_auto_reconnect_device();

        // Transition to Disconnected state
        // The retry loop checks state in addition to flag, so no sleep needed
        self.transition_to(ConnectionState::Disconnected);

        // Reset user_initiated_disconnect flag after disconnect is complete
        // The retry loop checks BOTH this flag AND state machine, so no race condition
        self.user_initiated_disconnect.set(false);
        web_sys::console::log_1(
            &"DEBUG: Resetting user_initiated_disconnect after disconnect() complete".into(),
        );

        // Release disconnect guard
        self.is_disconnecting.store(false, Ordering::SeqCst);

        true
    }

    fn capture_state(&self) -> ConnectionSnapshot {
        ConnectionSnapshot {
            baud: self.detected_baud.get_untracked(),
            framing: self.detected_framing.get_untracked(),
            decoder_id: self.decoder_id.get_untracked(),
        }
    }

    /// Restore UI state after failed reconfiguration
    /// NOTE: This is NOT a true transactional rollback - we cannot restore the connection
    /// after disconnect(). This only restores UI-facing state (detected baud/framing/decoder)
    /// for consistency after a failed baud rate change attempt.
    async fn restore_ui_state(&self, snapshot: ConnectionSnapshot) {
        web_sys::console::log_1(
            &format!(
                "Restoring UI state: baud={}, framing={}, decoder={}",
                snapshot.baud, snapshot.framing, snapshot.decoder_id
            )
            .into(),
        );

        // Restore detected config for UI consistency
        self.set_detected_baud.set(snapshot.baud);
        self.set_detected_framing.set(snapshot.framing);

        // Restore protocol decoder
        // This is important if reconfigure changed decoder (e.g., detected MAVLink)
        self.set_decoder(snapshot.decoder_id);

        // Do NOT restore connection_handle or set connected=true
        // The old connection is dead and cannot be revived
        // User must manually reconnect
    }

    // Reconfigure = Disconnect + Connect with Transaction Safety
    pub async fn reconfigure(&self, baud: u32, framing: &str) {
        // Only allow reconfigure if currently connected
        if self.state.get_untracked() != ConnectionState::Connected {
            return;
        }

        // Capture state before disconnect for potential rollback
        let snapshot = self.capture_state();

        // Save port reference before disconnect
        let port_opt = self.active_port.borrow().clone();

        // Disconnect while still in Connected state (required for disconnect() to succeed)
        // After disconnect(), we'll be in Disconnected state
        if !self.disconnect().await {
            // disconnect() failed - it already transitioned to appropriate state
            return;
        }

        // Note: We're now in Disconnected state. We skip transitioning to Reconfiguring
        // because Disconnected → Reconfiguring is not a valid transition. Instead, we'll
        // reconnect directly (Disconnected → Connecting → Connected). The reconfigure
        // operation is atomic from the user's perspective since we don't return until
        // reconnection completes or fails.

        if let Some(port) = port_opt {
            // Wait for port release
            let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(&mut |r, _| {
                if let Some(window) = web_sys::window() {
                    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&r, 100);
                }
            }))
            .await;

            // Detect config if needed
            let (final_baud, final_framing, initial_buf, proto) = if baud == 0 {
                let cached_auto = *self.last_auto_baud.borrow();

                if let Some(cached) = cached_auto {
                    let effective_framing = if framing == "Auto" { "8N1" } else { framing };
                    (cached, effective_framing.to_string(), None, None)
                } else {
                    let (b, f, buf, proto) = self.detect_config(port.clone(), framing).await;
                    // RACE CHECK: If disconnected during detection, abort
                    if self.active_port.borrow().is_none() {
                        // Transition to Disconnected state on abort
                        self.transition_to(ConnectionState::Disconnected);
                        return;
                    }
                    (b, f, Some(buf), proto)
                }
            } else if framing == "Auto" {
                let (detect_f, buf, proto) = self.smart_probe_framing(port.clone(), baud).await;
                if let Some(p) = proto.clone() {
                    self.set_decoder(p);
                }
                (baud, detect_f, Some(buf), proto)
            } else {
                (baud, framing.to_string(), None, None)
            };

            if let Some(p) = proto {
                self.set_decoder(p);
            }

            // Try to reconnect
            match self
                .connect_impl(port, final_baud, &final_framing, initial_buf)
                .await
            {
                Ok(_) => {
                    // connect_impl already transitioned to Connected
                    // Just update the status message
                    self.set_status.set("Reconfigured".into());
                }
                Err(e) => {
                    // Restore UI state (actual connection cannot be restored)
                    error_log!("Reconfigure failed: {}", e);
                    self.restore_ui_state(snapshot).await;
                    // Transition to Disconnected state on failure
                    self.transition_to(ConnectionState::Disconnected);
                    self.set_status.set(format!(
                        "Reconfigure failed: {}. Please reconnect manually.",
                        e
                    ));
                }
            }
        } else {
            // No port available, transition to Disconnected
            self.transition_to(ConnectionState::Disconnected);
        }
    }

    fn spawn_read_loop(
        &self,
        mut transport: WebSerialTransport,
        mut cmd_rx: mpsc::UnboundedReceiver<ConnectionCommand>,
        completion_tx: oneshot::Sender<()>,
    ) {
        let worker_signal = self.worker;
        let manager = self.clone();

        spawn_local(async move {
            web_sys::console::log_1(&"DEBUG: Read Loop STARTED".into());

            loop {
                // Use futures::select! to race between commands and reads
                select! {
                    cmd = cmd_rx.next() => {
                        match cmd {
                            Some(ConnectionCommand::Stop) => {
                                web_sys::console::log_1(&"DEBUG: Read Loop received STOP".into());
                                break;
                            }
                            Some(ConnectionCommand::Write { data, response }) => {
                                let result = transport.write(&data).await
                                    .map_err(|e| format!("TX Error: {:?}", e));
                                let is_ok = result.is_ok();
                                let _ = response.send(result);
                                if is_ok {
                                    manager.trigger_tx();
                                }
                            }
                            None => break, // Channel closed
                        }
                    }

                    read_result = transport.read_chunk().fuse() => {
                        match read_result {
                            Ok((chunk, ts)) if !chunk.is_empty() => {
                                // Send to worker
                                if let Some(w) = worker_signal.get_untracked() {
                                    let msg = UiToWorker::IngestData {
                                        data: chunk,
                                        timestamp_us: ts,
                                    };
                                    if let Ok(cmd_val) = serde_wasm_bindgen::to_value(&msg) {
                                        let _ = w.post_message(&cmd_val);
                                    }
                                }
                                manager.trigger_rx();
                            }
                            Err(e) => {
                                web_sys::console::log_1(
                                    &format!("DEBUG: Read Loop - Read Error: {:?}", e).into()
                                );
                                break;
                            }
                            _ => {
                                // Empty read - yield to prevent CPU spinning
                                //
                                // Without this, continuous empty reads cause 100% CPU usage.
                                // We use Promise.resolve() (microtask) instead of setTimeout (macrotask)
                                // because microtasks run at the end of the current event loop iteration,
                                // providing the fastest possible yield while still allowing the browser
                                // to process other events.
                                //
                                // This is the standard JavaScript async/await yield pattern.
                                let _ = wasm_bindgen_futures::JsFuture::from(
                                    js_sys::Promise::resolve(&wasm_bindgen::JsValue::UNDEFINED),
                                )
                                .await;
                            }
                        }
                    }
                }
            }

            web_sys::console::log_1(&"DEBUG: Read Loop EXITED".into());

            // CRITICAL: Call transport.close() to properly release reader/writer locks
            // Just dropping the transport doesn't call the JavaScript cancel/close methods
            web_sys::console::log_1(
                &"DEBUG: Calling transport.close() to release streams...".into(),
            );
            let _ = transport.close().await;
            web_sys::console::log_1(&"DEBUG: Transport closed, streams released".into());

            // Loop exited - notify manager of disconnection
            // Only transition to DeviceLost if we were Connected (not reconfiguring)
            let current_state = manager.state.get_untracked();
            if current_state == ConnectionState::Connected {
                // Transition to DeviceLost state
                manager.transition_to(ConnectionState::DeviceLost);
            }

            // Signal completion - streams are now released and port can be closed
            let _ = completion_tx.send(());
            web_sys::console::log_1(&"DEBUG: Read loop completion signaled".into());
        });
    }

    fn send_worker_config(&self, baud: u32) {
        if let Some(w) = self.worker.get_untracked() {
            let msg = UiToWorker::Connect { baud_rate: baud };
            if let Ok(cmd_val) = serde_wasm_bindgen::to_value(&msg) {
                let _ = w.post_message(&cmd_val);
            }
        }
    }
    pub async fn write(&self, data: &[u8]) -> Result<(), String> {
        // Clone only the cmd_tx sender (which is Clone), not the entire handle
        let cmd_tx = self
            .connection_handle
            .borrow()
            .as_ref()
            .map(|h| h.cmd_tx.clone());

        if let Some(tx) = cmd_tx {
            let (response_tx, rx) = oneshot::channel();

            if tx
                .unbounded_send(ConnectionCommand::Write {
                    data: data.to_vec(),
                    response: response_tx,
                })
                .is_err()
            {
                return Err("Connection closed".to_string());
            }

            // Wait for response
            match rx.await {
                Ok(result) => result,
                Err(_) => Err("Write timeout".to_string()),
            }
        } else {
            Err("Not connected".to_string())
        }
    }
    pub fn set_framer(&self, id: String) {
        if let Some(w) = self.worker.get_untracked() {
            let msg = UiToWorker::SetFramer { id: id.clone() };
            if let Ok(cmd_val) = serde_wasm_bindgen::to_value(&msg) {
                let _ = w.post_message(&cmd_val);
            }
        }
    }

    pub fn set_decoder(&self, id: String) {
        self.set_decoder_id.set(id.clone());
        if let Some(w) = self.worker.get_untracked() {
            let msg = UiToWorker::SetDecoder { id: id.clone() };
            if let Ok(cmd_val) = serde_wasm_bindgen::to_value(&msg) {
                let _ = w.post_message(&cmd_val);
            }

            // Auto-Start Heartbeat for MAVLink
            if id == "mavlink" {
                let msg_hb = UiToWorker::StartHeartbeat;
                if let Ok(val) = serde_wasm_bindgen::to_value(&msg_hb) {
                    let _ = w.post_message(&val);
                }
                web_sys::console::log_1(&"MAVLink Heartbeat Started".into());
            } else {
                let msg_hb = UiToWorker::StopHeartbeat;
                if let Ok(val) = serde_wasm_bindgen::to_value(&msg_hb) {
                    let _ = w.post_message(&val);
                }
            }
        }
    }

    pub async fn auto_select_port(
        &self,
        last_vid: Option<u16>,
        last_pid: Option<u16>,
    ) -> Option<web_sys::SerialPort> {
        let window = web_sys::window()?;
        let nav = window.navigator();
        let serial = nav.serial();

        if serial.is_undefined() {
            self.set_status
                .set("Error: WebSerial not supported.".into());
            return None;
        }

        if let Ok(ports_val) = wasm_bindgen_futures::JsFuture::from(serial.get_ports()).await {
            let ports: js_sys::Array = ports_val.unchecked_into();
            if ports.length() > 0 {
                // Priority: Match Last VID/PID
                let mut matched_port = None;
                if let (Some(l_vid), Some(l_pid)) = (last_vid, last_pid) {
                    for i in 0..ports.length() {
                        let p: web_sys::SerialPort = ports.get(i).unchecked_into();
                        let info = p.get_info();
                        let vid = js_sys::Reflect::get(&info, &"usbVendorId".into())
                            .ok()
                            .and_then(|v| v.as_f64())
                            .map(|v| v as u16);
                        let pid = js_sys::Reflect::get(&info, &"usbProductId".into())
                            .ok()
                            .and_then(|v| v.as_f64())
                            .map(|v| v as u16);
                        if vid == Some(l_vid) && pid == Some(l_pid) {
                            matched_port = Some(p);
                            self.set_status.set("Auto-selected known port...".into());
                            break;
                        }
                    }
                }

                // Fallback: If no match but only 1 port exists, use it
                if matched_port.is_none() && ports.length() == 1 {
                    matched_port = Some(ports.get(0).unchecked_into());
                    self.set_status
                        .set("Auto-selected single available port...".into());
                }
                return matched_port;
            }
        }
        None
    }

    pub async fn request_port(&self) -> Option<web_sys::SerialPort> {
        let window = web_sys::window()?;
        let nav = window.navigator();
        let serial = nav.serial();

        if serial.is_undefined() {
            self.set_status
                .set("Error: WebSerial not supported.".into());
            return None;
        }

        let options = js_sys::Object::new();
        // Filters removed to allow all devices (MAVLink/PX4 support)
        // let _ = js_sys::Reflect::set(&options, &"filters".into(), &js_sys::Array::new());

        match js_sys::Reflect::get(&serial, &"requestPort".into()) {
            Ok(func_val) => {
                let func: js_sys::Function = func_val.unchecked_into();
                match func.call1(&serial, &options) {
                    Ok(p) => {
                        match wasm_bindgen_futures::JsFuture::from(js_sys::Promise::from(p)).await {
                            Ok(val) => Some(val.unchecked_into()),
                            Err(_) => {
                                self.set_status.set("Cancelled".into());
                                None
                            }
                        }
                    }
                    Err(_) => {
                        self.set_status.set("Error: requestPort call failed".into());
                        None
                    }
                }
            }
            Err(_) => {
                self.set_status.set("Error: requestPort not found".into());
                None
            }
        }
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

    pub async fn detect_config(
        &self,
        port: web_sys::SerialPort,
        current_framing: &str,
    ) -> (u32, String, Vec<u8>, Option<String>) {
        // State machine already in Probing state (set by connect())
        // ondisconnect handler will ignore disconnects during Probing

        let baud_candidates = vec![
            115200, 1500000, 1000000, 2000000, 921600, 57600, 460800, 230400, 38400, 19200, 9600,
        ];
        let mut best_score = 0.0;
        let mut best_rate = 115200;
        let mut best_framing = "8N1".to_string();
        let mut best_buffer = Vec::new();
        let mut best_proto = None;

        // Helpers
        // Local scoring logic removed in favor of `analysis` crate.

        'outer: for rate in baud_candidates {
            // OPTIMIZATION: Abort probing early if disconnect detected
            // Once ondisconnect fires, subsequent probes will fail anyway
            if self.probing_interrupted.get() {
                web_sys::console::log_1(
                    &"DEBUG: Probing aborted - disconnect detected, remaining probes skipped"
                        .into(),
                );
                break 'outer;
            }

            self.set_status.set(format!("Scanning {}...", rate));
            web_sys::console::log_1(&format!("AUTO: Probing [v2] {}...", rate).into());
            let probe_start_ts = js_sys::Date::now();

            // 1. Probe 8N1
            let buffer = self
                .gather_probe_data(port.clone(), rate, "8N1", true)
                .await;
            let open_dur = js_sys::Date::now() - probe_start_ts;
            web_sys::console::log_1(
                &format!(
                    "PROFILE: Rate {} PROBED in {:.1}ms. Bytes: {}",
                    rate,
                    open_dur,
                    buffer.len()
                )
                .into(),
            );

            if buffer.is_empty() {
                continue;
            }

            // 2. Analyze
            let score_8n1 = analysis::calculate_score_8n1(&buffer);
            let score_7e1 = analysis::calculate_score_7e1(&buffer);
            let score_mav = analysis::calculate_score_mavlink(&buffer);

            web_sys::console::log_1(
                &format!(
                    "AUTO: Rate {} => 8N1: {:.4}, 7E1: {:.4}, MAV: {:.4} (Size: {})",
                    rate,
                    score_8n1,
                    score_7e1,
                    score_mav,
                    buffer.len()
                )
                .into(),
            );

            // MAVLink Priority Check (Robust)
            #[cfg(feature = "mavlink")]
            if self.verify_mavlink_integrity(&buffer) {
                best_score = 1.0;
                best_rate = rate;
                best_framing = "8N1".to_string(); // MAVLink is 8N1
                best_buffer = buffer.clone();
                best_proto = Some("mavlink".to_string());
                web_sys::console::log_1(
                    &"AUTO: MAVLink Verified (Magic+Parse)! Stopping probe.".into(),
                );
                break 'outer;
            }

            // Fallback to statistical score if verification inconclusive but score high
            if score_mav >= 0.99 {
                best_score = 1.0;
                best_rate = rate;
                best_framing = "8N1".to_string(); // MAVLink is 8N1
                best_buffer = buffer.clone();
                best_proto = Some("mavlink".to_string());
                web_sys::console::log_1(
                    &"AUTO: MAVLink Detected (Statistical). Stopping probe.".into(),
                );
                break 'outer;
            }

            if score_8n1 > best_score {
                best_score = score_8n1;
                best_rate = rate;
                best_framing = "8N1".to_string();
                best_buffer = buffer.clone();
            }
            if score_7e1 > best_score {
                best_score = score_7e1;
                best_rate = rate;
                best_framing = "7E1".to_string();
                best_buffer = buffer.clone();
            }

            // Optimization: If "Perfect" match found, stop scanning remaining rates
            if best_score > 0.99 && best_buffer.len() > 64 {
                web_sys::console::log_1(
                    &format!("AUTO: Perfect match found at {}. Stopping scan.", best_rate).into(),
                );
                *self.last_auto_baud.borrow_mut() = Some(best_rate); // SAVE CACHE
                break 'outer;
            }

            // High-Speed Optimization: Accept lower confidence for >= 1M baud
            // reasoning: High speed signals are sparse. If we see one, it's likely the right one.
            let threshold = if best_rate >= 1000000 { 0.85 } else { 0.98 };

            if best_score > threshold {
                web_sys::console::log_1(
                    &format!(
                        "AUTO: Early Break at {} (Score: {:.2} > {})",
                        best_rate, best_score, threshold
                    )
                    .into(),
                );
                *self.last_auto_baud.borrow_mut() = Some(best_rate); // SAVE CACHE
                break 'outer;
            }

            // 3. Fallback: Deep Probe if Auto Framing
            if current_framing == "Auto" && best_score < 0.5 {
                for fr in ["8E1", "8O1"] {
                    self.set_status
                        .set(format!("Deep Probe {} {}...", rate, fr));

                    let buf2 = self.gather_probe_data(port.clone(), rate, fr, true).await; // Use helper

                    // Original Deep Probe Logic Removed - replaced by helper call
                    let score = analysis::calculate_score_8n1(&buf2);
                    if score > best_score {
                        best_score = score;
                        best_rate = rate;
                        best_framing = fr.to_string();
                        best_buffer = buf2;
                    }
                    if score > 0.95 {
                        break 'outer;
                    }
                }
            }
        }

        self.set_status.set(format!(
            "Detected: {} {} (Score: {:.2})",
            best_rate, best_framing, best_score
        ));

        // State will be transitioned to Connecting/Connected by caller (connect_impl)

        (best_rate, best_framing, best_buffer, best_proto)
    }

    // Helper: Gather Probe Data (Extracted)
    async fn gather_probe_data(
        &self,
        port: web_sys::SerialPort,
        rate: u32,
        framing: &str,
        send_wakeup: bool,
    ) -> Vec<u8> {
        let mut t = WebSerialTransport::new();
        let (d, p, s) = Self::parse_framing(framing);
        let cfg = SerialConfig {
            baud_rate: rate,
            data_bits: d,
            parity: p,
            stop_bits: s,
            flow_control: "none".into(),
        };

        let mut buffer = Vec::new();
        // Retry Loop for "Port already open" race condition
        let mut attempts = 0;
        let success = loop {
            match t.open(port.clone(), cfg.clone()).await {
                Ok(_) => break true,
                Err(e) => {
                    let err_str = format!("{:?}", e);
                    if (err_str.contains("already open") || err_str.contains("InvalidStateError"))
                        && attempts < 3
                    {
                        attempts += 1;
                        // Wait 100ms
                        let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(
                            &mut |r, _| {
                                if let Some(window) = web_sys::window() {
                                    let _ = window
                                        .set_timeout_with_callback_and_timeout_and_arguments_0(
                                            &r, 100,
                                        );
                                }
                            },
                        ))
                        .await;
                        continue;
                    }
                    break false;
                }
            }
        };

        if success {
            // PASSIVE FIRST: Listen for 100ms
            let start_passive = js_sys::Date::now();
            let mut received_passive = false;
            while js_sys::Date::now() - start_passive < 100.0 {
                if let Ok((chunk, _)) = t.read_chunk().await {
                    if !chunk.is_empty() {
                        buffer.extend_from_slice(&chunk);
                        received_passive = true;
                    }
                } else {
                    break;
                }
                // Small breaks done by read_chunk internally usually
                if received_passive {
                    break;
                }
            }

            // IF NO DATA & WAKEUP REQUESTED -> Send Wakeup
            if !received_passive && buffer.is_empty() && send_wakeup {
                let _ = t.write(b"\r").await;
            }

            // Continue Reading (Active Phase or Extended Passive)
            let start_loop = js_sys::Date::now();
            let mut max_time = 50.0;

            while js_sys::Date::now() - start_loop < max_time {
                if let Ok((chunk, _)) = t.read_chunk().await {
                    if !chunk.is_empty() {
                        buffer.extend_from_slice(&chunk);
                        if max_time < 250.0 {
                            max_time = 250.0;
                        }
                        if buffer.len() > 64 {
                            // Use analysis crate if available or simple check (Assuming analysis
                            // crate in scope)
                            if analysis::calculate_score_8n1(&buffer) > 0.90 {
                                break;
                            }
                        }
                    }
                } else {
                    break;
                }
            }

            let _ = t.close().await;

            // Mandatory Cool-down: Give OS/Browser 200ms to release the lock completely
            let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(&mut |r, _| {
                if let Some(window) = web_sys::window() {
                    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&r, 200);
                }
            }))
            .await;
        }
        buffer
    }

    // Verify MAVLink Integrity (Magic Byte + Parse Test)
    #[cfg(feature = "mavlink")]
    fn verify_mavlink_integrity(&self, buffer: &[u8]) -> bool {
        // 1. Quick Magic Byte Scan
        let mut reader = buffer;
        loop {
            let magic_idx = reader.iter().position(|&b| b == 0xFE || b == 0xFD);
            if let Some(idx) = magic_idx {
                // Advance reader to magic
                if idx + 1 >= reader.len() {
                    return false;
                }
                let Some(&magic) = reader.get(idx) else {
                    return false;
                };
                // Make sure we have AT LEAST enough for a minimal valid header/payload
                // v1 min: 8 + 0 = 8. v2 min: 12 + 0 = 12.
                let min_packet_size = if magic == 0xFE { 8 } else { 12 };

                if idx + min_packet_size > reader.len() {
                    return false;
                }

                let Some(sub_slice) = reader.get(idx..) else {
                    return false;
                };

                // Try Parse
                let mut try_reader = sub_slice;
                let res = if magic == 0xFE {
                    mavlink::read_v1_msg::<mavlink::common::MavMessage, _>(&mut try_reader)
                } else {
                    mavlink::read_v2_msg::<mavlink::common::MavMessage, _>(&mut try_reader)
                };

                if res.is_ok() {
                    web_sys::console::log_1(
                        &format!("DEBUG: MAVLink VERIFIED. Magic: {:02X}", magic).into(),
                    );
                    return true; // Valid packet found!
                } else {
                    web_sys::console::log_1(
                        &format!(
                            "DEBUG: MAVLink Magic found but parse failed ({:?}).",
                            res.err()
                        )
                        .into(),
                    );
                }

                // Failed? Advance past this magic and retry
                let Some(next_reader) = reader.get(idx + 1..) else {
                    return false;
                };
                reader = next_reader;
            } else {
                return false;
            }
        }
    }

    // New: Smart Probe for Single Baud
    pub async fn smart_probe_framing(
        &self,
        port: web_sys::SerialPort,
        rate: u32,
    ) -> (String, Vec<u8>, Option<String>) {
        self.set_status.set(format!("Smart Probing {}...", rate));

        // 1. Probe 8N1 (Most common) - ACTIVE (Wakeup)
        let buf_8n1 = self
            .gather_probe_data(port.clone(), rate, "8N1", true)
            .await;
        if !buf_8n1.is_empty() {
            let score = analysis::calculate_score_8n1(&buf_8n1);
            let score_mav = analysis::calculate_score_mavlink(&buf_8n1);

            if score_mav >= 0.99 {
                return ("8N1".to_string(), buf_8n1, Some("mavlink".to_string()));
            }

            if score > 0.90 {
                return ("8N1".to_string(), buf_8n1, None);
            }
        }

        // 2. Probe 7E1 (Common alternative) - PASSIVE
        if !buf_8n1.is_empty() {
            // Only if we saw SOME data (garbage or not), try parity
            let buf_7e1 = self
                .gather_probe_data(port.clone(), rate, "7E1", true)
                .await;
            let score = analysis::calculate_score_7e1(&buf_7e1); // Assuming 7E1 score calc
            if score > 0.90 {
                return ("7E1".to_string(), buf_7e1, None);
            }
        }

        // Default to 8N1 if silent or unsure
        // Default to 8N1 if silent or unsure
        ("8N1".to_string(), buf_8n1, None)
    }
    // Internal connect implementation
    async fn connect_impl(
        &self,
        port: web_sys::SerialPort,
        baud: u32,
        framing: &str,
        initial_buffer: Option<Vec<u8>>,
    ) -> Result<(), String> {
        // Internal impl assumes is_connecting guard is handled by caller (connect)
        // or we are called by reconfigure (which handles its own state)

        let (d, p, s) = Self::parse_framing(framing);

        let cfg = SerialConfig {
            baud_rate: baud,
            data_bits: d,
            parity: p,
            stop_bits: s,
            flow_control: "none".into(),
        };

        let mut t = WebSerialTransport::new();

        // Retry Loop for "Port already open" race condition (probe cleanup lag)
        let mut attempts = 0;
        let result = loop {
            match t.open(port.clone(), cfg.clone()).await {
                Ok(_) => {
                    // CREATE channel for commands
                    let (cmd_tx, cmd_rx) = mpsc::unbounded();

                    // CREATE oneshot channel for read loop completion signal
                    let (completion_tx, completion_rx) = oneshot::channel();

                    // Store handle
                    *self.connection_handle.borrow_mut() = Some(ConnectionHandle {
                        cmd_tx: cmd_tx.clone(),
                        completion_rx,
                    });
                    *self.active_port.borrow_mut() = Some(port);

                    // Transition to Connected state
                    self.transition_to(ConnectionState::Connected);

                    // Update detected config for UI
                    self.set_detected_baud.set(baud);
                    self.set_detected_framing.set(framing.to_string());

                    // Spawn read loop with command channel and completion signal
                    self.spawn_read_loop(t, cmd_rx, completion_tx);

                    // Notify Worker
                    self.send_worker_config(baud);

                    // Replay Initial Buffer (if any)
                    if let Some(buf) = initial_buffer {
                        if !buf.is_empty() {
                            // Strip leading CR/LF/Whitespace (cleanup wakeup echo)
                            let start_idx = buf
                                .iter()
                                .position(|&x| x != b'\r' && x != b'\n')
                                .unwrap_or(0);
                            let clean_buf = if let Some(slice) = buf.get(start_idx..) {
                                slice.to_vec()
                            } else {
                                Vec::new()
                            };

                            if !clean_buf.is_empty() {
                                if let Some(w) = self.worker.get_untracked() {
                                    let msg = UiToWorker::IngestData {
                                        data: clean_buf,
                                        timestamp_us: (js_sys::Date::now() * 1000.0) as u64,
                                    };
                                    if let Ok(cmd_val) = serde_wasm_bindgen::to_value(&msg) {
                                        let _ = w.post_message(&cmd_val);
                                    }
                                }
                            }
                        }
                    }

                    break Ok(());
                }
                Err(e) => {
                    let err_str = format!("{:?}", e);
                    if (err_str.contains("already open") || err_str.contains("InvalidStateError"))
                        && attempts < 10
                    {
                        attempts += 1;
                        web_sys::console::warn_1(
                            &format!(
                                "Connection blocked by busy port. Retrying ({}/10)...",
                                attempts
                            )
                            .into(),
                        );

                        // Wait 200ms
                        let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(
                            &mut |r, _| {
                                if let Some(window) = web_sys::window() {
                                    let _ = window
                                        .set_timeout_with_callback_and_timeout_and_arguments_0(
                                            &r, 200,
                                        );
                                }
                            },
                        ))
                        .await;
                        continue;
                    }

                    self.set_status.set(format!("Connection Failed: {:?}", e));
                    break Err(format!("{:?}", e));
                }
            }
        };

        // Guard cleared by caller
        result
    }

    // --- Auto-Reconnect Logic ---
    #[allow(clippy::too_many_arguments)]
    pub fn setup_auto_reconnect(
        &self,
        last_vid: Signal<Option<u16>>,
        last_pid: Signal<Option<u16>>,
        set_last_vid: WriteSignal<Option<u16>>,
        set_last_pid: WriteSignal<Option<u16>>,
        baud_signal: Signal<u32>,
        detected_baud: Signal<u32>,
        framing_signal: Signal<String>,
    ) {
        // Store VID/PID write signals for use in disconnect()
        *self.set_last_vid.borrow_mut() = Some(set_last_vid);
        *self.set_last_pid.borrow_mut() = Some(set_last_pid);

        let manager_conn = self.clone();
        let manager_disc = self.clone();

        let on_connect_closure = Closure::wrap(Box::new(move |_e: web_sys::Event| {
            let t_onconnect = js_sys::Date::now();
            web_sys::console::log_1(
                &format!("[{}ms] serial.onconnect triggered", t_onconnect as u64).into(),
            );

            // OPTIMIZATION: Ignore auto-reconnect if manual connection already in progress
            // This prevents noisy state transitions when user manually connects
            let current_state = manager_conn.state.get_untracked();
            if matches!(
                current_state,
                ConnectionState::Probing | ConnectionState::Connecting
            ) {
                web_sys::console::log_1(
                    &format!(
                        "[{}ms] Ignoring onconnect - manual connection in progress (state: {:?})",
                        (js_sys::Date::now() - t_onconnect) as u64,
                        current_state
                    )
                    .into(),
                );
                return;
            }

            // Check if it matches our last device
            let vid_opt = last_vid.get_untracked();
            let pid_opt = last_pid.get_untracked();
            web_sys::console::log_1(
                &format!(
                    "[{}ms] last_vid={:?}, last_pid={:?}",
                    (js_sys::Date::now() - t_onconnect) as u64,
                    vid_opt,
                    pid_opt
                )
                .into(),
            );

            if let (Some(target_vid), Some(target_pid)) = (vid_opt, pid_opt) {
                let manager_conn = manager_conn.clone();
                spawn_local(async move {
                    let t0 = js_sys::Date::now();
                    let Some(window) = web_sys::window() else {
                        return;
                    };
                    let nav = window.navigator();
                    let serial = nav.serial();
                    // getPorts() returns Promise directly
                    let promise = serial.get_ports();

                    web_sys::console::log_1(
                        &format!(
                            "[{}ms] Calling getPorts()...",
                            (js_sys::Date::now() - t0) as u64
                        )
                        .into(),
                    );

                    if let Ok(val) = wasm_bindgen_futures::JsFuture::from(promise).await {
                        let ports: js_sys::Array = val.unchecked_into();
                        web_sys::console::log_1(
                            &format!(
                                "[{}ms] Scanning {} ports for reconnect",
                                (js_sys::Date::now() - t0) as u64,
                                ports.length()
                            )
                            .into(),
                        );

                        for i in 0..ports.length() {
                            let p: web_sys::SerialPort = ports.get(i).unchecked_into();
                            let info = p.get_info();
                            let vid = js_sys::Reflect::get(&info, &"usbVendorId".into())
                                .ok()
                                .and_then(|v| v.as_f64())
                                .map(|v| v as u16);
                            let pid = js_sys::Reflect::get(&info, &"usbProductId".into())
                                .ok()
                                .and_then(|v| v.as_f64())
                                .map(|v| v as u16);

                            web_sys::console::log_1(
                                &format!(
                                    "DEBUG: Port {}: vid={:?} pid={:?} (looking for vid={} pid={})",
                                    i, vid, pid, target_vid, target_pid
                                )
                                .into(),
                            );

                            if vid == Some(target_vid) && pid == Some(target_pid) {
                                web_sys::console::log_1(
                                    &format!(
                                        "[{}ms] Matched device! vid={:?} pid={:?}",
                                        (js_sys::Date::now() - t0) as u64,
                                        vid,
                                        pid
                                    )
                                    .into(),
                                );
                                // Transition to AutoReconnecting state
                                manager_conn.transition_to(ConnectionState::AutoReconnecting);

                                // We reuse the `options` / `baud`
                                // Use default framing for auto-reconnect (or derived from valid
                                // config)
                                let user_pref_baud = baud_signal.get_untracked();
                                let last_known_baud = detected_baud.get_untracked();

                                // SMART RECONNECT:
                                // If user meant "Auto" (0), but we successfully connected before
                                // (last_known > 0), reuse that rate
                                // to avoid a full re-scan (which sends '\r' and takes time).
                                let target_baud = if user_pref_baud == 0 && last_known_baud > 0 {
                                    last_known_baud
                                } else if user_pref_baud == 0 {
                                    115200 // Fallback if no history (shouldn't happen on reconnect
                                           // usually)
                                } else {
                                    user_pref_baud
                                };

                                let current_framing = framing_signal.get_untracked();
                                let final_framing_str = if current_framing == "Auto" {
                                    "8N1".to_string()
                                } else {
                                    current_framing
                                };

                                let (_d_r, _p_r, _s_r) =
                                    ConnectionManager::parse_framing(&final_framing_str);

                                // Manager Connect (Handles open, loop, worker)
                                // Auto-reconnect not needed here, handled by manager internal state
                                // or explicit loop

                                spawn_local(async move {
                                    // Check if already connecting (without holding lock)
                                    if manager_conn.is_connecting.load(Ordering::SeqCst) {
                                        web_sys::console::log_1(
                                            &"Reconnect skipped - already connecting".into(),
                                        );
                                        return; // Exit immediately
                                    }

                                    let t_reconnect = js_sys::Date::now();
                                    web_sys::console::log_1(
                                        &format!(
                                            "[{}ms] Auto-reconnect - calling disconnect()",
                                            t_reconnect as u64
                                        )
                                        .into(),
                                    );

                                    // FORCE RESET: Close any stale handles (even if we think we are
                                    // disconnected, the browser might hold the lock)
                                    // CRITICAL FIX: Exit task if disconnect() was skipped (another
                                    // task already running)
                                    if !manager_conn.disconnect().await {
                                        web_sys::console::log_1(
                                            &"DEBUG: Auto-reconnect - exiting task, disconnect \
                                              already handled by another task"
                                                .into(),
                                        );
                                        return; // Exit entire reconnection task
                                    }

                                    // CRITICAL FIX: Reset user_initiated flag immediately
                                    // disconnect() sets this flag to true, but auto-reconnect is
                                    // NOT user-initiated
                                    // If we don't reset it before next physical disconnect, that
                                    // disconnect will be
                                    // incorrectly treated as user-initiated, disabling
                                    // auto-reconnect
                                    manager_conn.user_initiated_disconnect.set(false);
                                    web_sys::console::log_1(
                                        &"DEBUG: Auto-reconnect - reset user_initiated flag to \
                                          false"
                                            .into(),
                                    );

                                    web_sys::console::log_1(
                                        &format!(
                                            "[{}ms] Auto-reconnect - disconnect() completed, \
                                             waiting 100ms",
                                            (js_sys::Date::now() - t_reconnect) as u64
                                        )
                                        .into(),
                                    );

                                    // OPTIMIZATION: Reduced wait from 500ms→200ms→100ms
                                    // Skip-close optimization makes disconnect() much faster
                                    let _ = wasm_bindgen_futures::JsFuture::from(
                                            js_sys::Promise::new(&mut |r, _| {
                                                if let Some(window) = web_sys::window() {
                                                    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&r, 100);
                                                }
                                            })
                                        ).await;

                                    web_sys::console::log_1(
                                        &format!(
                                            "[{}ms] Auto-reconnect - calling connect() with \
                                             baud={}",
                                            (js_sys::Date::now() - t_reconnect) as u64,
                                            target_baud
                                        )
                                        .into(),
                                    );

                                    // Manager updates status signals automatically
                                    if let Err(e) = manager_conn
                                        .connect(p, target_baud, &final_framing_str)
                                        .await
                                    {
                                        // OPTIMIZATION: Don't log "Already connecting" as failure
                                        // (expected race)
                                        if e == "Already connecting" {
                                            web_sys::console::log_1(
                                                &format!(
                                                    "[{}ms] Auto-reconnect - another path already \
                                                     connecting, exiting gracefully",
                                                    (js_sys::Date::now() - t_reconnect) as u64
                                                )
                                                .into(),
                                            );
                                        } else {
                                            web_sys::console::log_1(
                                                &format!(
                                                    "[{}ms] Auto-reconnect FAILED: {:?}",
                                                    (js_sys::Date::now() - t_reconnect) as u64,
                                                    e
                                                )
                                                .into(),
                                            );
                                        }
                                    } else {
                                        let elapsed = (js_sys::Date::now() - t_reconnect) as u64;
                                        web_sys::console::log_1(
                                            &format!(
                                                "[{}ms] Auto-reconnect SUCCESS (total {}ms)",
                                                elapsed, elapsed
                                            )
                                            .into(),
                                        );
                                        // connect() already transitioned to Connected
                                        // Just update the status message for "Restored" vs
                                        // "Connected"
                                        manager_conn.set_status.set("Restored Connection".into());
                                    }
                                });
                                return; // Stop checking
                            }
                        }
                        // PERFORMANCE FIX: Retry getPorts() instead of waiting for next onconnect
                        // On USB re-insertion, browser may enumerate wrong device first,
                        // then correct device appears shortly after
                        web_sys::console::log_1(
                            &format!(
                                "[{}ms] No matching device found (target vid={}, pid={}), \
                                 retrying...",
                                (js_sys::Date::now() - t0) as u64,
                                target_vid,
                                target_pid
                            )
                            .into(),
                        );

                        // Transition to AutoReconnecting state for UI visual feedback (pulsing
                        // orange light)
                        manager_conn.transition_to(ConnectionState::AutoReconnecting);

                        // PERFORMANCE FIX: Retry up to 200 times with 50ms intervals (10000ms max)
                        // Extended from 120 retries (6s) to handle worst-case OS device
                        // enumeration. Analysis: Most USB enumerations
                        // complete in 2-6s, worst case is 8-10s.
                        // On USB re-insertion, OS may enumerate wrong device first (e.g.,
                        // VID=12642), then correct device appears 2-8
                        // seconds later (e.g., VID=7052). User testing
                        // showed 6s window missed device by only 164ms in edge cases.
                        // This 10s window provides adequate buffer while maintaining
                        // responsiveness. If device still not found after
                        // max timeout, serial.onconnect fallback still applies.
                        for retry_attempt in 1..=AUTO_RECONNECT_MAX_RETRIES {
                            // OPTIMIZATION: Exit retry loop if another path already started
                            // reconnecting This happens when a new
                            // serial.onconnect event finds device while retry loop is still running
                            if manager_conn.is_connecting.load(Ordering::SeqCst) {
                                let current_state = manager_conn.state.get_untracked();
                                web_sys::console::log_1(
                                    &format!(
                                        "[{}ms] Retry loop - exiting early, reconnection already \
                                         started by another path (state={:?})",
                                        (js_sys::Date::now() - t0) as u64,
                                        current_state
                                    )
                                    .into(),
                                );
                                // Only transition if still in AutoReconnecting state
                                // If already Connected, don't overwrite with DeviceLost (prevents
                                // UI flicker)
                                if current_state == ConnectionState::AutoReconnecting {
                                    manager_conn.transition_to(ConnectionState::DeviceLost);
                                }
                                return;
                            }

                            // Exit if user canceled via disconnect button
                            // Check BOTH flag AND state to avoid race condition:
                            // - Flag check: Fast path for immediate detection
                            // - State check: Reliable check if flag was already reset
                            let user_canceled = manager_conn.user_initiated_disconnect.get();
                            let current_state = manager_conn.state.get_untracked();

                            if user_canceled || current_state == ConnectionState::Disconnected {
                                web_sys::console::log_1(
                                    &format!(
                                        "[Auto-reconnect] Aborted (flag={}, state={:?})",
                                        user_canceled, current_state
                                    )
                                    .into(),
                                );

                                // Only transition if not already Disconnected
                                if current_state != ConnectionState::Disconnected {
                                    manager_conn.transition_to(ConnectionState::Disconnected);
                                }

                                // Clear VID/PID to prevent auto-reconnect on next insertion
                                manager_conn.clear_auto_reconnect_device();
                                return;
                            }

                            // Wait before retry
                            let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(
                                &mut |r, _| {
                                    if let Some(window) = web_sys::window() {
                                        let _ = window
                                            .set_timeout_with_callback_and_timeout_and_arguments_0(
                                                &r,
                                                AUTO_RECONNECT_RETRY_INTERVAL_MS,
                                            );
                                    }
                                },
                            ))
                            .await;

                            // DIAGNOSTIC: Log progress every 10 retries to track polling activity
                            if retry_attempt % 10 == 0 {
                                web_sys::console::log_1(
                                    &format!(
                                        "[{}ms] Retry attempt {}/200 - still polling for device \
                                         (target vid={}, pid={})...",
                                        (js_sys::Date::now() - t0) as u64,
                                        retry_attempt,
                                        target_vid,
                                        target_pid
                                    )
                                    .into(),
                                );
                            }

                            // Retry getPorts()
                            let promise_retry = serial.get_ports();
                            if let Ok(val_retry) =
                                wasm_bindgen_futures::JsFuture::from(promise_retry).await
                            {
                                let ports_retry: js_sys::Array = val_retry.unchecked_into();

                                for i in 0..ports_retry.length() {
                                    let p: web_sys::SerialPort =
                                        ports_retry.get(i).unchecked_into();
                                    let info = p.get_info();
                                    let vid = js_sys::Reflect::get(&info, &"usbVendorId".into())
                                        .ok()
                                        .and_then(|v| v.as_f64())
                                        .map(|v| v as u16);
                                    let pid = js_sys::Reflect::get(&info, &"usbProductId".into())
                                        .ok()
                                        .and_then(|v| v.as_f64())
                                        .map(|v| v as u16);

                                    if vid == Some(target_vid) && pid == Some(target_pid) {
                                        web_sys::console::log_1(
                                            &format!(
                                                "[{}ms] Matched device on retry {}! vid={:?} \
                                                 pid={:?}",
                                                (js_sys::Date::now() - t0) as u64,
                                                retry_attempt,
                                                vid,
                                                pid
                                            )
                                            .into(),
                                        );

                                        // Same reconnection logic as above
                                        // Transition to AutoReconnecting state
                                        manager_conn
                                            .transition_to(ConnectionState::AutoReconnecting);

                                        let user_pref_baud = baud_signal.get_untracked();
                                        let last_known_baud = detected_baud.get_untracked();
                                        let target_baud =
                                            if user_pref_baud == 0 && last_known_baud > 0 {
                                                last_known_baud
                                            } else if user_pref_baud == 0 {
                                                115200
                                            } else {
                                                user_pref_baud
                                            };

                                        let current_framing = framing_signal.get_untracked();
                                        let final_framing_str = if current_framing == "Auto" {
                                            "8N1".to_string()
                                        } else {
                                            current_framing
                                        };

                                        let manager_conn_clone = manager_conn.clone();
                                        spawn_local(async move {
                                            // Check if already connecting (without holding lock)
                                            if manager_conn_clone
                                                .is_connecting
                                                .load(Ordering::SeqCst)
                                            {
                                                web_sys::console::log_1(
                                                    &"Reconnect skipped - already connecting"
                                                        .into(),
                                                );
                                                return; // Exit immediately
                                            }

                                            let t_reconnect = js_sys::Date::now();
                                            web_sys::console::log_1(
                                                &format!(
                                                    "[{}ms] Auto-reconnect - calling disconnect()",
                                                    t_reconnect as u64
                                                )
                                                .into(),
                                            );

                                            // CRITICAL FIX: Exit task if disconnect() was skipped
                                            // (another task already running)
                                            if !manager_conn_clone.disconnect().await {
                                                web_sys::console::log_1(
                                                    &"DEBUG: Auto-reconnect - exiting task, \
                                                      disconnect already handled by another task"
                                                        .into(),
                                                );
                                                return; // Exit entire reconnection task
                                            }

                                            manager_conn_clone.user_initiated_disconnect.set(false);
                                            web_sys::console::log_1(
                                                &"DEBUG: Auto-reconnect - reset user_initiated \
                                                  flag to false"
                                                    .into(),
                                            );

                                            web_sys::console::log_1(
                                                &format!(
                                                    "[{}ms] Auto-reconnect - disconnect() \
                                                     completed, waiting 100ms",
                                                    (js_sys::Date::now() - t_reconnect) as u64
                                                )
                                                .into(),
                                            );

                                            let _ = wasm_bindgen_futures::JsFuture::from(
                                                    js_sys::Promise::new(&mut |r, _| {
                                                        if let Some(window) = web_sys::window() {
                                                            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&r, 100);
                                                        }
                                                    })
                                                ).await;

                                            web_sys::console::log_1(
                                                &format!(
                                                    "[{}ms] Auto-reconnect - calling connect() \
                                                     with baud={}",
                                                    (js_sys::Date::now() - t_reconnect) as u64,
                                                    target_baud
                                                )
                                                .into(),
                                            );

                                            if let Err(e) = manager_conn_clone
                                                .connect(p, target_baud, &final_framing_str)
                                                .await
                                            {
                                                // OPTIMIZATION: Don't log "Already connecting" as
                                                // failure (expected race)
                                                if e == "Already connecting" {
                                                    web_sys::console::log_1(
                                                        &format!(
                                                            "[{}ms] Auto-reconnect - another path \
                                                             already connecting, exiting \
                                                             gracefully",
                                                            (js_sys::Date::now() - t_reconnect)
                                                                as u64
                                                        )
                                                        .into(),
                                                    );
                                                } else {
                                                    web_sys::console::log_1(
                                                        &format!(
                                                            "[{}ms] Auto-reconnect FAILED: {:?}",
                                                            (js_sys::Date::now() - t_reconnect)
                                                                as u64,
                                                            e
                                                        )
                                                        .into(),
                                                    );
                                                }
                                            } else {
                                                let elapsed =
                                                    (js_sys::Date::now() - t_reconnect) as u64;
                                                web_sys::console::log_1(
                                                    &format!(
                                                        "[{}ms] Auto-reconnect SUCCESS (total \
                                                         {}ms)",
                                                        elapsed, elapsed
                                                    )
                                                    .into(),
                                                );
                                                // connect() already transitioned to Connected
                                                // Just update the status message for "Restored" vs
                                                // "Connected"
                                                manager_conn_clone
                                                    .set_status
                                                    .set("Restored Connection".into());
                                            }
                                        });
                                        return;
                                    }
                                }
                            }
                        }

                        // Transition back to DeviceLost after retry loop ends
                        manager_conn.transition_to(ConnectionState::DeviceLost);

                        web_sys::console::log_1(
                            &format!(
                                "[{}ms] No matching device found after 200 retries (target \
                                 vid={}, pid={}). Waiting for serial.onconnect event...",
                                (js_sys::Date::now() - t0) as u64,
                                target_vid,
                                target_pid
                            )
                            .into(),
                        );
                    }
                });
            } else {
                web_sys::console::log_1(&"DEBUG: No last_vid/last_pid to match against".into());
            }
        }) as Box<dyn FnMut(_)>);

        // On Disconnect
        let on_disconnect_closure = Closure::wrap(Box::new(move |_e: web_sys::Event| {
            web_sys::console::log_1(&"DEBUG: serial.ondisconnect triggered".into());

            // Ignore disconnect events during reconfiguring
            let current_state = manager_disc.state.get_untracked();
            if current_state == ConnectionState::Reconfiguring {
                web_sys::console::log_1(
                    &format!("DEBUG: ignoring disconnect (state: {:?})", current_state).into(),
                );
                return;
            }

            // Track disconnect events during probing (don't change state, but flag for cleanup)
            if current_state == ConnectionState::Probing {
                web_sys::console::warn_1(
                    &"DEBUG: ondisconnect fired during Probing - marking probe as interrupted"
                        .into(),
                );
                manager_disc.probing_interrupted.set(true);
                return;
            }

            // DEBOUNCE: If already Disconnected, this is a duplicate ondisconnect event
            // disconnect() has already handled cleanup, so ignore this event
            // This prevents flag from being reset prematurely by first event,
            // causing second event to be treated as device loss
            if current_state == ConnectionState::Disconnected {
                web_sys::console::log_1(
                    &"DEBUG: ondisconnect fired but already Disconnected - ignoring duplicate \
                      event"
                        .into(),
                );
                return;
            }

            // Check if this was a user-initiated disconnect
            let user_initiated = manager_disc.user_initiated_disconnect.get();

            if user_initiated {
                web_sys::console::log_1(
                    &"DEBUG: User-initiated disconnect - disabling auto-reconnect".into(),
                );
                // Clear VID/PID to prevent auto-reconnect
                manager_disc.clear_auto_reconnect_device();
                // Transition to Disconnected state
                manager_disc.transition_to(ConnectionState::Disconnected);
                // Reset the flag for next time
                manager_disc.user_initiated_disconnect.set(false);
            } else {
                web_sys::console::log_1(
                    &"DEBUG: Device disconnected (not user-initiated) - will auto-reconnect".into(),
                );
                // Keep last_vid/last_pid to enable auto-reconnect
                // Transition to DeviceLost state
                manager_disc.transition_to(ConnectionState::DeviceLost);
                // Don't set is_auto_reconnecting yet - wait for retry loop to start
                // This creates steady orange light while waiting for device insertion
                // Once device is inserted and retry loop starts, it will pulse
            }
        }) as Box<dyn FnMut(_)>);

        // Attach listeners
        let Some(window) = web_sys::window() else {
            return;
        };
        let nav = window.navigator();
        let serial = nav.serial();

        if !serial.is_undefined() {
            serial.set_onconnect(Some(on_connect_closure.as_ref().unchecked_ref()));
            serial.set_ondisconnect(Some(on_disconnect_closure.as_ref().unchecked_ref()));

            // MEMORY LEAK FIX: Store closures instead of leaking them
            // If setup_auto_reconnect() is called multiple times (e.g., reconnect with
            // auto-detect), the old closures are properly dropped and cleaned up
            let old_closures = self
                .event_closures
                .borrow_mut()
                .replace((on_connect_closure, on_disconnect_closure));

            if old_closures.is_some() {
                web_sys::console::log_1(
                    &"DEBUG: Replaced old event handlers (prevented memory leak)".into(),
                );
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_framing_8n1() {
        let (d, p, s) = ConnectionManager::parse_framing("8N1");
        assert_eq!(d, 8);
        assert_eq!(p, "none");
        assert_eq!(s, 1);
    }

    #[test]
    fn test_parse_framing_7e1() {
        let (d, p, s) = ConnectionManager::parse_framing("7E1");
        assert_eq!(d, 7);
        assert_eq!(p, "even");
        assert_eq!(s, 1);
    }

    #[test]
    fn test_parse_framing_8o2() {
        let (d, p, s) = ConnectionManager::parse_framing("8O2");
        assert_eq!(d, 8);
        assert_eq!(p, "odd");
        assert_eq!(s, 2);
    }

    #[test]
    fn test_atomic_connection_guard() {
        let guard = Arc::new(AtomicBool::new(false));

        // First acquire succeeds
        assert!(guard
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok());

        // Second acquire fails (already true)
        assert!(guard
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_err());

        // Release
        guard.store(false, Ordering::SeqCst);

        // Can acquire again
        assert!(guard
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok());
    }

    // ============ State Machine Transition Tests ============

    /// Test all valid state transitions are allowed
    #[test]
    fn test_valid_state_transitions() {
        use ConnectionState::*;

        // From Disconnected
        assert!(Disconnected.can_transition_to(Probing));
        assert!(Disconnected.can_transition_to(Connecting));
        assert!(Disconnected.can_transition_to(Disconnected)); // Idempotent

        // From Probing
        assert!(Probing.can_transition_to(Connecting));
        assert!(Probing.can_transition_to(Disconnected));
        assert!(Probing.can_transition_to(AutoReconnecting));

        // From Connecting
        assert!(Connecting.can_transition_to(Connected));
        assert!(Connecting.can_transition_to(Disconnected));
        assert!(Connecting.can_transition_to(DeviceLost));

        // From Connected
        assert!(Connected.can_transition_to(Reconfiguring));
        assert!(Connected.can_transition_to(Disconnecting));
        assert!(Connected.can_transition_to(DeviceLost));
        assert!(Connected.can_transition_to(AutoReconnecting));

        // From Reconfiguring
        assert!(Reconfiguring.can_transition_to(Connected));
        assert!(Reconfiguring.can_transition_to(DeviceLost));

        // From Disconnecting
        assert!(Disconnecting.can_transition_to(Disconnected));

        // From DeviceLost
        assert!(DeviceLost.can_transition_to(Disconnected));
        assert!(DeviceLost.can_transition_to(AutoReconnecting));
        assert!(DeviceLost.can_transition_to(Connecting));

        // From AutoReconnecting
        assert!(AutoReconnecting.can_transition_to(Connected));
        assert!(AutoReconnecting.can_transition_to(DeviceLost));
        assert!(AutoReconnecting.can_transition_to(Disconnected));
        assert!(AutoReconnecting.can_transition_to(AutoReconnecting)); // Idempotent
    }

    /// Test invalid state transitions are rejected
    /// This is critical for fail-fast behavior - we want to catch bugs early
    #[test]
    fn test_invalid_state_transitions() {
        use ConnectionState::*;

        // Cannot go directly from Disconnected to Connected (must go through Connecting)
        assert!(!Disconnected.can_transition_to(Connected));
        assert!(!Disconnected.can_transition_to(DeviceLost));
        assert!(!Disconnected.can_transition_to(AutoReconnecting));
        assert!(!Disconnected.can_transition_to(Reconfiguring));
        assert!(!Disconnected.can_transition_to(Disconnecting));

        // Cannot disconnect while probing (must cancel probing first)
        assert!(!Probing.can_transition_to(Disconnecting));
        assert!(!Probing.can_transition_to(DeviceLost));
        assert!(!Probing.can_transition_to(Connected));
        assert!(!Probing.can_transition_to(Reconfiguring));

        // Cannot bypass connection process
        assert!(!Connecting.can_transition_to(Probing));
        assert!(!Connecting.can_transition_to(Reconfiguring));
        assert!(!Connecting.can_transition_to(Disconnecting));
        assert!(!Connecting.can_transition_to(AutoReconnecting));

        // Cannot skip disconnecting process
        assert!(!Connected.can_transition_to(Disconnected));
        assert!(!Connected.can_transition_to(Probing));
        assert!(!Connected.can_transition_to(Connecting));

        // Reconfiguring is a transient state - limited exits
        assert!(!Reconfiguring.can_transition_to(Disconnected));
        assert!(!Reconfiguring.can_transition_to(Disconnecting));
        assert!(!Reconfiguring.can_transition_to(Probing));
        assert!(!Reconfiguring.can_transition_to(Connecting));
        assert!(!Reconfiguring.can_transition_to(AutoReconnecting));
        assert!(!Reconfiguring.can_transition_to(Reconfiguring));

        // Disconnecting is one-way to Disconnected
        assert!(!Disconnecting.can_transition_to(Connected));
        assert!(!Disconnecting.can_transition_to(Connecting));
        assert!(!Disconnecting.can_transition_to(Probing));
        assert!(!Disconnecting.can_transition_to(DeviceLost));
        assert!(!Disconnecting.can_transition_to(AutoReconnecting));
        assert!(!Disconnecting.can_transition_to(Reconfiguring));
        assert!(!Disconnecting.can_transition_to(Disconnecting));

        // DeviceLost cannot go to intermediate states
        assert!(!DeviceLost.can_transition_to(Probing));
        assert!(!DeviceLost.can_transition_to(Reconfiguring));
        assert!(!DeviceLost.can_transition_to(Disconnecting));
        assert!(!DeviceLost.can_transition_to(DeviceLost));

        // AutoReconnecting cannot skip states
        assert!(!AutoReconnecting.can_transition_to(Probing));
        assert!(!AutoReconnecting.can_transition_to(Connecting));
        assert!(!AutoReconnecting.can_transition_to(Reconfiguring));
        assert!(!AutoReconnecting.can_transition_to(Disconnecting));
    }

    /// Test state permission queries
    #[test]
    fn test_state_permissions() {
        use ConnectionState::*;

        // Only Connected, AutoReconnecting, and DeviceLost can disconnect
        assert!(!Disconnected.can_disconnect());
        assert!(!Probing.can_disconnect());
        assert!(!Connecting.can_disconnect());
        assert!(Connected.can_disconnect());
        assert!(!Reconfiguring.can_disconnect());
        assert!(!Disconnecting.can_disconnect());
        assert!(DeviceLost.can_disconnect());
        assert!(AutoReconnecting.can_disconnect());
    }

    /// Test indicator colors match expected values
    #[test]
    fn test_indicator_colors() {
        use ConnectionState::*;

        assert_eq!(Connected.indicator_color(), "rgb(95, 200, 85)"); // Green
        assert_eq!(Disconnected.indicator_color(), "rgb(240, 105, 95)"); // Red
        assert_eq!(DeviceLost.indicator_color(), "rgb(245, 190, 80)"); // Orange
        assert_eq!(AutoReconnecting.indicator_color(), "rgb(245, 190, 80)"); // Orange
        assert_eq!(Connecting.indicator_color(), "rgb(245, 190, 80)"); // Orange
        assert_eq!(Probing.indicator_color(), "rgb(245, 190, 80)"); // Orange
        assert_eq!(Reconfiguring.indicator_color(), "rgb(245, 190, 80)"); // Orange
        assert_eq!(Disconnecting.indicator_color(), "rgb(245, 190, 80)"); // Orange
    }

    /// Test indicator pulse behavior
    #[test]
    fn test_indicator_pulse() {
        use ConnectionState::*;

        // Only AutoReconnecting should pulse
        assert!(!Disconnected.indicator_should_pulse());
        assert!(!Probing.indicator_should_pulse());
        assert!(!Connecting.indicator_should_pulse());
        assert!(!Connected.indicator_should_pulse());
        assert!(!Reconfiguring.indicator_should_pulse());
        assert!(!Disconnecting.indicator_should_pulse());
        assert!(!DeviceLost.indicator_should_pulse());
        assert!(AutoReconnecting.indicator_should_pulse());
    }

    /// Test button text logic
    #[test]
    fn test_button_text() {
        use ConnectionState::*;

        // Button should show "Disconnect" for these states
        assert!(!Disconnected.button_shows_disconnect());
        assert!(!Probing.button_shows_disconnect());
        assert!(!Connecting.button_shows_disconnect());
        assert!(Connected.button_shows_disconnect());
        assert!(!Reconfiguring.button_shows_disconnect());
        assert!(!Disconnecting.button_shows_disconnect());
        assert!(DeviceLost.button_shows_disconnect());
        assert!(AutoReconnecting.button_shows_disconnect());
    }

    /// Test status text strings
    #[test]
    fn test_status_text() {
        use ConnectionState::*;

        assert_eq!(Disconnected.status_text(), "Ready to connect");
        assert_eq!(Probing.status_text(), "Detecting firmware...");
        assert_eq!(Connecting.status_text(), "Connecting...");
        assert_eq!(Connected.status_text(), "Connected");
        assert_eq!(Reconfiguring.status_text(), "Reconfiguring...");
        assert_eq!(Disconnecting.status_text(), "Disconnecting...");
        assert_eq!(
            DeviceLost.status_text(),
            "Device Lost (waiting for device...)"
        );
        assert_eq!(
            AutoReconnecting.status_text(),
            "Device found. Auto-reconnecting..."
        );
    }

    // ============ Timing Constants Tests ============

    /// Test timing constants are sensible
    #[test]
    fn test_timing_constants() {
        // Polling intervals should be increasing (adaptive backoff)
        assert!(POLL_INTERVAL_FAST_MS < POLL_INTERVAL_MEDIUM_MS);
        assert!(POLL_INTERVAL_MEDIUM_MS < POLL_INTERVAL_SLOW_MS);

        // Polling should be faster than auto-reconnect total timeout
        let max_polling_time = (5 * POLL_INTERVAL_FAST_MS)
            + (5 * POLL_INTERVAL_MEDIUM_MS)
            + (5 * POLL_INTERVAL_SLOW_MS);
        let auto_reconnect_timeout =
            AUTO_RECONNECT_MAX_RETRIES as i32 * AUTO_RECONNECT_RETRY_INTERVAL_MS;
        assert!(max_polling_time < auto_reconnect_timeout);

        // Disconnect timeout should be reasonable (not too short, not too long)
        assert!(DISCONNECT_COMPLETION_TIMEOUT_MS >= 1000); // At least 1 second
        assert!(DISCONNECT_COMPLETION_TIMEOUT_MS <= 5000); // At most 5 seconds

        // Wait times should be positive
        assert!(REENUMERATION_WAIT_MS > 0);
        assert!(PROBING_CLEANUP_WAIT_MS > 0);
        assert!(AUTO_RECONNECT_RETRY_INTERVAL_MS > 0);
    }

    // ============ Edge Case Tests ============

    /// Test that all states have unique colors or pulse behavior
    /// This ensures UI can distinguish between all states
    #[test]
    fn test_states_visually_distinguishable() {
        use ConnectionState::*;

        // Connected: green, no pulse
        assert_eq!(Connected.indicator_color(), "rgb(95, 200, 85)");
        assert!(!Connected.indicator_should_pulse());

        // Disconnected: red, no pulse
        assert_eq!(Disconnected.indicator_color(), "rgb(240, 105, 95)");
        assert!(!Disconnected.indicator_should_pulse());

        // AutoReconnecting: orange, PULSE (unique!)
        assert_eq!(AutoReconnecting.indicator_color(), "rgb(245, 190, 80)");
        assert!(AutoReconnecting.indicator_should_pulse());

        // All other orange states: no pulse
        let orange_states = vec![
            DeviceLost,
            Connecting,
            Probing,
            Reconfiguring,
            Disconnecting,
        ];
        for state in orange_states {
            assert_eq!(state.indicator_color(), "rgb(245, 190, 80)");
            assert!(!state.indicator_should_pulse());
        }
    }

    /// Test exhaustive state enumeration
    /// This ensures we didn't forget to handle any state
    #[test]
    fn test_all_states_covered() {
        use ConnectionState::*;

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

        // Verify all states have status text
        for state in &all_states {
            assert!(!state.status_text().is_empty());
        }

        // Verify all states have indicator color
        for state in &all_states {
            assert!(!state.indicator_color().is_empty());
        }

        // Verify can_disconnect is defined for all states
        for state in &all_states {
            let _ = state.can_disconnect(); // Just call it to ensure no panic
        }

        // Verify button_shows_disconnect is defined for all states
        for state in &all_states {
            let _ = state.button_shows_disconnect();
        }
    }

    /// Test that idempotent transitions are allowed
    /// Some states should allow transitioning to themselves (e.g., retry loops)
    #[test]
    fn test_idempotent_transitions() {
        use ConnectionState::*;

        // Disconnected can transition to itself (no-op)
        assert!(Disconnected.can_transition_to(Disconnected));

        // AutoReconnecting can transition to itself (retry loop)
        assert!(AutoReconnecting.can_transition_to(AutoReconnecting));

        // All other states should NOT allow self-transition
        assert!(!Probing.can_transition_to(Probing));
        assert!(!Connecting.can_transition_to(Connecting));
        assert!(!Connected.can_transition_to(Connected));
        assert!(!Reconfiguring.can_transition_to(Reconfiguring));
        assert!(!Disconnecting.can_transition_to(Disconnecting));
        assert!(!DeviceLost.can_transition_to(DeviceLost));
    }

    /// Test critical path: disconnect must be possible from device-loss states
    /// This is the bug fix from Issue 1 - ensure it stays fixed
    #[test]
    fn test_disconnect_after_device_loss() {
        use ConnectionState::*;

        // CRITICAL: User must be able to cancel waiting/reconnecting
        assert!(DeviceLost.can_disconnect());
        assert!(AutoReconnecting.can_disconnect());

        // Verify transition path: DeviceLost → Disconnected
        assert!(DeviceLost.can_transition_to(Disconnected));

        // Verify transition path: AutoReconnecting → Disconnected
        assert!(AutoReconnecting.can_transition_to(Disconnected));

        // Button should show "Disconnect" in these states
        assert!(DeviceLost.button_shows_disconnect());
        assert!(AutoReconnecting.button_shows_disconnect());
    }

    /// Test that Disconnecting cannot be interrupted
    /// Once disconnection starts, it must complete to Disconnected
    #[test]
    fn test_disconnecting_is_one_way() {
        use ConnectionState::*;

        // Only valid transition is to Disconnected
        assert!(Disconnecting.can_transition_to(Disconnected));

        // All other transitions must be blocked
        assert!(!Disconnecting.can_transition_to(Connected));
        assert!(!Disconnecting.can_transition_to(Connecting));
        assert!(!Disconnecting.can_transition_to(Probing));
        assert!(!Disconnecting.can_transition_to(DeviceLost));
        assert!(!Disconnecting.can_transition_to(AutoReconnecting));
        assert!(!Disconnecting.can_transition_to(Reconfiguring));
        assert!(!Disconnecting.can_transition_to(Disconnecting));

        // User cannot disconnect while already disconnecting
        assert!(!Disconnecting.can_disconnect());
    }

    /// Test USB unplug scenarios from different states
    #[test]
    fn test_usb_unplug_transitions() {
        use ConnectionState::*;

        // USB unplug while connected
        assert!(Connected.can_transition_to(DeviceLost));
        assert!(Connected.can_transition_to(AutoReconnecting));

        // USB unplug while connecting
        assert!(Connecting.can_transition_to(DeviceLost));

        // USB unplug while reconfiguring
        assert!(Reconfiguring.can_transition_to(DeviceLost));

        // Cannot unplug from states where device is already gone
        assert!(!Disconnected.can_transition_to(DeviceLost));
        assert!(!DeviceLost.can_transition_to(DeviceLost));
    }

    /// Test probing interrupt recovery path
    #[test]
    fn test_probing_interrupt_path() {
        use ConnectionState::*;

        // Probing can be interrupted by device reconnect
        assert!(Probing.can_transition_to(AutoReconnecting));

        // After interrupt, can proceed to connecting
        assert!(AutoReconnecting.can_transition_to(Connected));

        // Or fall back to device lost
        assert!(AutoReconnecting.can_transition_to(DeviceLost));
    }

    /// Test that transient states cannot be long-lived
    /// Disconnecting and Reconfiguring should only transition to stable states
    #[test]
    fn test_transient_states() {
        use ConnectionState::*;

        // Disconnecting is transient - only goes to Disconnected
        let disconnecting_exits = vec![
            Disconnected,
            Probing,
            Connecting,
            Connected,
            Reconfiguring,
            DeviceLost,
            AutoReconnecting,
        ];
        for target in &disconnecting_exits {
            if *target == Disconnected {
                assert!(Disconnecting.can_transition_to(*target));
            } else {
                assert!(!Disconnecting.can_transition_to(*target));
            }
        }

        // Reconfiguring is transient - only goes to Connected or DeviceLost
        let reconfiguring_exits = vec![
            Disconnected,
            Probing,
            Connecting,
            Connected,
            Reconfiguring,
            Disconnecting,
            AutoReconnecting,
        ];
        for target in &reconfiguring_exits {
            if *target == Connected || *target == DeviceLost {
                assert!(Reconfiguring.can_transition_to(*target));
            } else {
                assert!(!Reconfiguring.can_transition_to(*target));
            }
        }
    }

    // ============ Concurrent Access Tests ============

    /// Test that connection guard prevents concurrent connection attempts
    /// This is critical for preventing "port already open" errors
    #[test]
    fn test_concurrent_connection_guard() {
        let is_connecting = Arc::new(AtomicBool::new(false));

        // Simulate first connection attempt
        let guard1_acquired = is_connecting
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok();
        assert!(guard1_acquired, "First connection should acquire guard");

        // Simulate concurrent connection attempt
        let guard2_acquired = is_connecting
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok();
        assert!(
            !guard2_acquired,
            "Second concurrent connection should fail to acquire guard"
        );

        // Verify flag is still set
        assert!(is_connecting.load(Ordering::SeqCst));

        // Release guard (using scopeguard in real code)
        is_connecting.store(false, Ordering::SeqCst);

        // Third attempt after release should succeed
        let guard3_acquired = is_connecting
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok();
        assert!(guard3_acquired, "Connection after release should succeed");
    }

    /// Test that disconnect guard prevents concurrent disconnect attempts
    #[test]
    fn test_concurrent_disconnect_guard() {
        let is_disconnecting = Arc::new(AtomicBool::new(false));

        // First disconnect attempt
        let guard1_acquired = is_disconnecting
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok();
        assert!(guard1_acquired);

        // Concurrent disconnect attempt should fail
        let guard2_acquired = is_disconnecting
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok();
        assert!(!guard2_acquired);

        // Cleanup
        is_disconnecting.store(false, Ordering::SeqCst);
    }

    /// Test that user_initiated_disconnect flag has correct semantics
    #[test]
    fn test_user_initiated_disconnect_flag() {
        // Flag should distinguish user action from device loss
        let user_flag = Rc::new(Cell::new(false));

        // User clicks disconnect
        user_flag.set(true);
        assert!(user_flag.get(), "Flag should be set on user action");

        // Flag should be checked before auto-reconnect
        let should_auto_reconnect = !user_flag.get();
        assert!(
            !should_auto_reconnect,
            "Should not auto-reconnect if user initiated"
        );

        // Flag should be cleared after disconnect completes
        user_flag.set(false);
        assert!(!user_flag.get(), "Flag should be cleared after disconnect");

        // Device loss (not user initiated)
        let should_auto_reconnect2 = !user_flag.get();
        assert!(
            should_auto_reconnect2,
            "Should auto-reconnect on device loss"
        );
    }

    /// Test probing_interrupted flag logic
    #[test]
    fn test_probing_interrupted_flag() {
        let probing_interrupted = Rc::new(Cell::new(false));

        // Initial state: not interrupted
        assert!(!probing_interrupted.get());

        // Probing starts with auto-detect (baud == 0)
        // Flag is cleared
        probing_interrupted.set(false);
        assert!(!probing_interrupted.get());

        // ondisconnect fires during probing
        probing_interrupted.set(true);
        assert!(
            probing_interrupted.get(),
            "Flag should be set on disconnect"
        );

        // Polling code checks flag
        let skip_polling = probing_interrupted.get();
        assert!(skip_polling, "Should skip polling if interrupted");

        // Next auto-detect attempt clears flag
        probing_interrupted.set(false);
        assert!(!probing_interrupted.get());
    }

    // ============ Parse Framing Edge Cases ============

    /// Test parse_framing with invalid inputs
    #[test]
    fn test_parse_framing_invalid() {
        // Too short
        let (d, p, s) = ConnectionManager::parse_framing("8N");
        assert_eq!(d, 8);
        assert_eq!(p, "none");
        assert_eq!(s, 1); // Should default to 1

        // Empty string
        let (d, p, s) = ConnectionManager::parse_framing("");
        assert_eq!(d, 8); // Default data bits
        assert_eq!(p, "none"); // Default parity
        assert_eq!(s, 1); // Default stop bits

        // Invalid parity character
        let (d, p, s) = ConnectionManager::parse_framing("8X1");
        assert_eq!(d, 8);
        assert_eq!(p, "none"); // Should default to none
        assert_eq!(s, 1);

        // Lowercase parity character (case-sensitive, only uppercase supported)
        let (d, p, s) = ConnectionManager::parse_framing("7e2");
        assert_eq!(d, 7);
        assert_eq!(p, "none"); // Lowercase 'e' not recognized, defaults to none
        assert_eq!(s, 2);
    }

    /// Test parse_framing with all valid parity options
    #[test]
    fn test_parse_framing_all_parity() {
        // None
        let (_, p, _) = ConnectionManager::parse_framing("8N1");
        assert_eq!(p, "none");

        // Even
        let (_, p, _) = ConnectionManager::parse_framing("8E1");
        assert_eq!(p, "even");

        // Odd
        let (_, p, _) = ConnectionManager::parse_framing("8O1");
        assert_eq!(p, "odd");

        // Mark and Space are not currently supported, default to none
        let (_, p, _) = ConnectionManager::parse_framing("8M1");
        assert_eq!(p, "none"); // Not supported, defaults to none

        let (_, p, _) = ConnectionManager::parse_framing("8S1");
        assert_eq!(p, "none"); // Not supported, defaults to none
    }

    /// Test parse_framing with different data bits
    #[test]
    fn test_parse_framing_data_bits() {
        // 5 data bits
        let (d, _, _) = ConnectionManager::parse_framing("5N1");
        assert_eq!(d, 5);

        // 6 data bits
        let (d, _, _) = ConnectionManager::parse_framing("6N1");
        assert_eq!(d, 6);

        // 7 data bits
        let (d, _, _) = ConnectionManager::parse_framing("7N1");
        assert_eq!(d, 7);

        // 8 data bits (most common)
        let (d, _, _) = ConnectionManager::parse_framing("8N1");
        assert_eq!(d, 8);

        // 9 data bits (uncommon but valid)
        let (d, _, _) = ConnectionManager::parse_framing("9N1");
        assert_eq!(d, 9);
    }

    /// Test parse_framing with different stop bits
    #[test]
    fn test_parse_framing_stop_bits() {
        // 1 stop bit (most common)
        let (_, _, s) = ConnectionManager::parse_framing("8N1");
        assert_eq!(s, 1);

        // 2 stop bits
        let (_, _, s) = ConnectionManager::parse_framing("8N2");
        assert_eq!(s, 2);

        // parse_framing parses any digit directly (no validation)
        // So "8N3" will parse as 3, even though it's not a standard stop bit value
        let (_, _, s) = ConnectionManager::parse_framing("8N3");
        assert_eq!(s, 3); // Parsed as-is (no validation in parse_framing)

        // Invalid stop bit character defaults to 1
        let (_, _, s) = ConnectionManager::parse_framing("8NX");
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
