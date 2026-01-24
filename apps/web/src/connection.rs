pub mod driver;
pub mod prober;
pub mod reconnect;
pub mod types;

// Re-export public types for external use
pub use types::*;

use self::prober::{detect_config, smart_probe_framing};
use crate::protocol::UiToWorker;
use futures_channel::oneshot;
use leptos::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::Worker;

/// USB device re-enumeration typically takes 100-300ms on most operating systems
/// after physical disconnect. We wait 200ms to ensure the device has completed
/// its re-initialization before attempting to access it.
const REENUMERATION_WAIT_MS: i32 = 200;

/// After probing interruption, wait for probe buffers and event handlers to drain
/// before checking the probing_interrupted flag. This prevents race conditions
/// where the flag is checked before the ondisconnect handler sets it.
const PROBING_CLEANUP_WAIT_MS: i32 = 150;

/// Polling intervals for device re-enumeration detection.
/// Fast interval (first 2 attempts): 100ms for responsive user experience
const POLL_INTERVAL_FAST_MS: i32 = 100;

/// Medium interval (next 5 attempts): 200ms balances responsiveness with CPU usage
const POLL_INTERVAL_MEDIUM_MS: i32 = 200;

/// Slow interval (remaining attempts): 400ms reduces CPU usage for extended waits
const POLL_INTERVAL_SLOW_MS: i32 = 400;

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
            atomic_state: Arc::new(AtomicConnectionState::new(ConnectionState::Disconnected)),

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
    ///
    /// CRITICAL: This updates BOTH atomic_state AND signal to keep them synchronized.
    /// If atomic_state is locked (operation in progress), it updates the state value
    /// while preserving the lock bit.
    pub fn transition_to(&self, new_state: ConnectionState) {
        // CRITICAL FIX: Read from atomic_state (source of truth), NOT signal
        let old_state = self.atomic_state.get();

        if !old_state.can_transition_to(new_state) {
            // Log warning but don't panic - this might be a valid reactive transition
            web_sys::console::warn_1(
                &format!(
                    "State transition: {:?} -> {:?} (not in state machine, but allowing for reactive events)",
                    old_state, new_state
                )
                .into(),
            );
        }

        web_sys::console::log_1(
            &format!("State transition: {:?} -> {:?}", old_state, new_state).into(),
        );

        // CRITICAL: Update BOTH atomic_state AND signal to keep them synchronized
        // This method preserves the lock bit if currently locked
        self.atomic_state.set_preserving_lock(new_state);

        // Update signal to match
        self.set_state.set(new_state);
        self.set_status.set(new_state.status_text().into());
    }

    // ============ State Transition API ============
    //
    // Two types of state changes:
    //
    // 1. EXCLUSIVE TRANSITIONS (need lock)
    //    Use: begin_exclusive_transition()
    //    Examples: connect(), disconnect(), reconfigure()
    //    Pattern: Lock atomic + sync signal + return guard
    //
    // 2. REACTIVE TRANSITIONS (no lock)
    //    Use: transition_to()
    //    Examples: USB unplug detected, read error, auto-reconnect retry
    //    Pattern: Update signal only (atomic unchanged or already set)
    //
    // CRITICAL: All validation uses atomic_state as source of truth.
    // Signals are derived views, allowed to lag briefly during transitions.

    /// Begin exclusive transition (lock + sync signal)
    ///
    /// This is the PRIMARY method for operations that need exclusive access:
    /// - connect()
    /// - disconnect()
    /// - reconfigure()
    ///
    /// Returns guard that ensures both atomic and signal are synchronized
    /// even if panic occurs.
    ///
    /// # Usage
    /// ```
    /// let guard = self.begin_exclusive_transition(current, Connecting)?;
    /// // ... do work ...
    /// guard.finish(Connected); // Success path
    /// // Or: drop(guard); // Error path (auto-unlocks to Disconnected)
    /// ```
    pub fn begin_exclusive_transition(
        &self,
        from: ConnectionState,
        to: ConnectionState,
    ) -> Result<ExclusiveTransitionGuard, String> {
        // Step 1: Lock atomic state
        self.atomic_state
            .try_transition(from, to)
            .map_err(|actual| format!("Transition blocked (state: {:?})", actual))?;

        // Step 2: IMMEDIATELY update signal to match
        // (now guaranteed to happen atomically with lock acquisition)
        self.set_state.set(to);
        self.set_status.set(to.status_text().into());

        // Step 3: Return guard for cleanup
        Ok(ExclusiveTransitionGuard {
            atomic_state: Arc::clone(&self.atomic_state),
            set_state: self.set_state,
            set_status: self.set_status,
        })
    }

    /// Acquire operation lock without forcing state transition
    ///
    /// This is for orchestrator methods like connect() that need to prevent
    /// concurrent operations but delegate state management to sub-methods.
    ///
    /// Returns a guard that only manages the lock bit - sub-methods are free
    /// to call transition_to() to update states as needed.
    ///
    /// # Usage
    /// ```
    /// let _lock = self.acquire_operation_lock()?;
    /// // Sub-methods can call transition_to() freely
    /// self.connect_with_auto_detect(port, framing).await?;
    /// // Lock automatically released on Drop
    /// ```
    pub fn acquire_operation_lock(&self) -> Result<OperationLockGuard, String> {
        self.atomic_state
            .try_lock()
            .map(|_| OperationLockGuard {
                atomic_state: Arc::clone(&self.atomic_state),
            })
            .map_err(|state| format!("Operation already in progress (state: {:?})", state))
    }

    pub fn get_status(&self) -> Signal<String> {
        self.status
    }

    pub fn clear_auto_reconnect_device(&self) {
        if let Some(set_vid) = *self.set_last_vid.borrow() {
            set_vid.set(None);
        }
        if let Some(set_pid) = *self.set_last_pid.borrow() {
            set_pid.set(None);
        }
        #[cfg(debug_assertions)]
        web_sys::console::log_1(&"DEBUG: Cleared VID/PID to prevent auto-reconnect".into());
    }

    // --- Connection Orchestration ---

    pub async fn connect(
        &self,
        port: web_sys::SerialPort,
        baud: u32,
        framing: &str,
    ) -> Result<(), String> {
        // Acquire lock without forcing state transition
        // Sub-methods will manage their own state transitions via transition_to()
        let _lock = self.acquire_operation_lock()?;

        if self.active_port.borrow().is_some() {
            return Err("Already connected".to_string());
        }

        if baud == 0 {
            self.probing_interrupted.set(false);
        }

        // Sub-methods handle state transitions (Connecting, Probing, Connected, etc.)
        let result = if baud > 0 && framing == "Auto" {
            self.connect_with_smart_framing(port, baud).await
        } else if baud == 0 {
            self.connect_with_auto_detect(port, framing).await
        } else {
            // Direct connect path - set Connecting state first
            self.transition_to(ConnectionState::Connecting);
            match self.connect_impl(port, baud, framing, None).await {
                Ok(()) => Ok(()),
                Err(e) => {
                    self.transition_to(ConnectionState::Disconnected);
                    Err(e)
                }
            }
        };

        // Lock automatically released on Drop
        // Note: State is already set to Connected by sub-methods on success,
        // or reverted to Disconnected on error
        result
    }

    async fn connect_with_smart_framing(
        &self,
        port: web_sys::SerialPort,
        baud: u32,
    ) -> Result<(), String> {
        self.transition_to(ConnectionState::Connecting);

        let (detect_framing, initial_buf, proto) =
            smart_probe_framing(port.clone(), baud, self.set_status).await;

        if let Some(p) = proto {
            self.set_decoder(p);
        }

        match self
            .connect_impl(port, baud, &detect_framing, Some(initial_buf))
            .await
        {
            Ok(()) => Ok(()),
            Err(e) => {
                self.transition_to(ConnectionState::Disconnected);
                Err(e)
            }
        }
    }

    async fn connect_with_auto_detect(
        &self,
        port: web_sys::SerialPort,
        framing: &str,
    ) -> Result<(), String> {
        self.transition_to(ConnectionState::Probing);

        let (baud, framing_str, buffer, proto) = detect_config(
            port.clone(),
            framing,
            self.set_status,
            self.probing_interrupted.clone(),
            self.last_auto_baud.clone(),
        )
        .await;

        // Handle interruption
        let recovery_result = self
            .handle_probing_with_interrupt_recovery(port, baud, framing_str, buffer, proto)
            .await;

        match recovery_result {
            Ok((final_port, final_baud, final_framing, initial_buffer, detected_proto)) => {
                if let Some(p) = detected_proto {
                    self.set_decoder(p);
                }

                // Check user intent FIRST (before any state transitions)
                if self.user_initiated_disconnect.get() {
                    #[cfg(debug_assertions)]
                    {
                        web_sys::console::log_1(
                            &"Connection aborted - user initiated disconnect".into(),
                        );
                    }
                    self.transition_to(ConnectionState::Disconnected);
                    return Err("User cancelled".into());
                }

                let current_state = self.atomic_state.get();
                if current_state == ConnectionState::Probing
                    || current_state == ConnectionState::Disconnected
                {
                    // Double-check user intent (TOCTOU protection)
                    if self.user_initiated_disconnect.get() {
                        self.transition_to(ConnectionState::Disconnected);
                        return Err("User cancelled".into());
                    }

                    self.transition_to(ConnectionState::Connecting);
                    match self
                        .connect_impl(final_port, final_baud, &final_framing, initial_buffer)
                        .await
                    {
                        Ok(()) => Ok(()),
                        Err(e) => {
                            self.transition_to(ConnectionState::Disconnected);
                            Err(e)
                        }
                    }
                } else {
                    self.transition_to(ConnectionState::Disconnected);
                    Err("Connection aborted".into())
                }
            }
            Err(e) => {
                // Transition to Disconnected when user cancels or error occurs
                self.transition_to(ConnectionState::Disconnected);
                Err(e)
            }
        }
    }

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
        let was_interrupted = self.probing_interrupted.get();
        if was_interrupted {
            // Check if this was user-initiated disconnect vs device unplug
            if self.user_initiated_disconnect.get() {
                #[cfg(debug_assertions)]
                web_sys::console::log_1(
                    &"DEBUG: Probing interrupted by user disconnect - aborting connection".into(),
                );
                self.probing_interrupted.set(false);
                return Err("User cancelled".to_string());
            }

            web_sys::console::warn_1(
                &"Probing was interrupted by ondisconnect event - port reference is stale".into(),
            );
            self.probing_interrupted.set(false);

            match self.poll_for_device_reenumeration().await {
                Ok(fresh_port) => Ok((fresh_port, baud, framing, Some(buffer), proto)),
                Err(e) => {
                    // Status will be set by transition_to(Disconnected) in caller
                    Err(e)
                }
            }
        } else {
            let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(&mut |r, _| {
                if let Some(window) = web_sys::window() {
                    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                        &r,
                        PROBING_CLEANUP_WAIT_MS,
                    );
                }
            }))
            .await;

            if self.probing_interrupted.get() {
                // Check if this was user-initiated disconnect vs device unplug
                if self.user_initiated_disconnect.get() {
                    #[cfg(debug_assertions)]
                    web_sys::console::log_1(
                        &"DEBUG: Probing interrupted by user disconnect - aborting connection"
                            .into(),
                    );
                    self.probing_interrupted.set(false);
                    return Err("User cancelled".to_string());
                }

                self.probing_interrupted.set(false);
                match self.poll_for_device_reenumeration().await {
                    Ok(fresh_port) => Ok((fresh_port, baud, framing, Some(buffer), proto)),
                    Err(e) => {
                        // Status will be set by transition_to(Disconnected) in caller
                        Err(e)
                    }
                }
            } else {
                Ok((port, baud, framing, Some(buffer), proto))
            }
        }
    }

    async fn poll_for_device_reenumeration(&self) -> Result<web_sys::SerialPort, String> {
        let Some(window) = web_sys::window() else {
            return Err("Window not available".to_string());
        };
        let nav = window.navigator();
        let serial = nav.serial();

        // Simple polling loop
        let mut fresh_port: Option<web_sys::SerialPort> = None;

        for attempt in 1..=15 {
            match wasm_bindgen_futures::JsFuture::from(serial.get_ports()).await {
                Ok(ports_val) => {
                    let ports: js_sys::Array = ports_val.unchecked_into();
                    if ports.length() > 0 {
                        fresh_port = Some(ports.get(0).unchecked_into());
                        break;
                    }
                }
                Err(_) => return Err("Cannot access ports".to_string()),
            }

            let wait_ms = if attempt <= 5 {
                POLL_INTERVAL_FAST_MS
            } else if attempt <= 10 {
                POLL_INTERVAL_MEDIUM_MS
            } else {
                POLL_INTERVAL_SLOW_MS
            };

            let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(&mut |r, _| {
                if let Some(window) = web_sys::window() {
                    let _ =
                        window.set_timeout_with_callback_and_timeout_and_arguments_0(&r, wait_ms);
                }
            }))
            .await;
        }

        if let Some(port) = fresh_port {
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
            Err("Device not found after disconnect".to_string())
        }
    }

    pub fn send_worker_config(&self, baud: u32) {
        if let Some(w) = self.worker.get_untracked() {
            let msg = UiToWorker::Connect { baud_rate: baud };
            if let Ok(cmd_val) = serde_wasm_bindgen::to_value(&msg) {
                let _ = w.post_message(&cmd_val);
            }
        }
    }

    pub async fn write(&self, data: &[u8]) -> Result<(), String> {
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

            if id == "mavlink" {
                let msg_hb = UiToWorker::StartHeartbeat;
                if let Ok(val) = serde_wasm_bindgen::to_value(&msg_hb) {
                    let _ = w.post_message(&val);
                }
            } else {
                let msg_hb = UiToWorker::StopHeartbeat;
                if let Ok(val) = serde_wasm_bindgen::to_value(&msg_hb) {
                    let _ = w.post_message(&val);
                }
            }
        }
    }

    // Auto-select port helper (exposed for UI)
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

        let promise = serial.request_port();
        match wasm_bindgen_futures::JsFuture::from(promise).await {
            Ok(val) => Some(val.unchecked_into()),
            Err(_) => None,
        }
    }
}
