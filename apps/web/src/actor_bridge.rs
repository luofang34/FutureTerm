use actor_protocol::{ConnectionState, SerialPortInfo, SystemEvent, UiCommand};
use actor_runtime::ChannelManager;
use futures::stream::StreamExt;
use leptos::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::Worker;

/// ActorBridge adapts the actor system to Leptos reactive signals
///
/// **Replaces ConnectionManager** while maintaining API compatibility.
///
/// This is the only component that touches Leptos signals, keeping all
/// business logic (actors) framework-agnostic.
///
/// ## Architecture
///
/// ```text
/// UI Components
///     ↓ (call methods)
/// ActorBridge
///     ↓ (send UiCommand via channel)
/// Actor System
///     ↓ (emit SystemEvent via channel)
/// ActorBridge Event Loop
///     ↓ (update signals + forward to worker)
/// UI Components (reactive updates)
/// ```
#[derive(Clone)]
#[allow(dead_code)] // API compatibility fields may not all be used yet
pub struct ActorBridge {
    // Channel for sending commands to actors
    manager: ChannelManager,

    // Reactive state for UI
    pub state: ReadSignal<ConnectionState>,
    set_state: WriteSignal<ConnectionState>,

    pub status: ReadSignal<String>,
    pub set_status: WriteSignal<String>,

    pub detected_baud: ReadSignal<u32>,
    pub set_detected_baud: WriteSignal<u32>,

    pub detected_framing: ReadSignal<String>,
    pub set_detected_framing: WriteSignal<String>,

    pub rx_active: ReadSignal<bool>,
    set_rx_active: WriteSignal<bool>,

    pub tx_active: ReadSignal<bool>,
    set_tx_active: WriteSignal<bool>,

    pub decoder_id: ReadSignal<String>,
    set_decoder_id: WriteSignal<String>,

    pub framer_id: ReadSignal<String>,
    set_framer_id: WriteSignal<String>,

    // Worker for data processing
    worker_signal: Signal<Option<Worker>>,

    // Store last connected port for reconfigure
    #[cfg(target_arch = "wasm32")]
    last_port: std::rc::Rc<std::cell::RefCell<Option<web_sys::SerialPort>>>,
}

/// Helper function to send messages to worker with error handling
fn send_to_worker(
    worker: &Worker,
    msg: crate::protocol::UiToWorker,
    set_status: WriteSignal<String>,
) {
    match serde_wasm_bindgen::to_value(&msg) {
        Ok(msg_js) => {
            if let Err(e) = worker.post_message(&msg_js) {
                #[cfg(debug_assertions)]
                web_sys::console::error_1(&format!("Worker post_message failed: {:?}", e).into());
                set_status.set(format!("Worker error: {:?}", e));
            }
        }
        Err(e) => {
            #[cfg(debug_assertions)]
            web_sys::console::error_1(
                &format!("Worker message serialization failed: {:?}", e).into(),
            );
            set_status.set(format!("Serialization error: {:?}", e));
        }
    }
}

#[allow(dead_code)] // API compatibility methods may not all be used yet
impl ActorBridge {
    /// Create a new ActorBridge
    ///
    /// This spawns the event loop that converts SystemEvents to signal updates
    pub fn new(mut manager: ChannelManager, worker_signal: Signal<Option<Worker>>) -> Self {
        let (state, set_state) = create_signal(ConnectionState::Disconnected);
        let (status, set_status) = create_signal("Ready".to_string());
        let (detected_baud, set_detected_baud) = create_signal(0u32);
        let (detected_framing, set_detected_framing) = create_signal(String::new());
        let (rx_active, set_rx_active) = create_signal(false);
        let (tx_active, set_tx_active) = create_signal(false);
        let (decoder_id, set_decoder_id) = create_signal("utf8".to_string());
        let (framer_id, set_framer_id) = create_signal("raw".to_string());

        // Spawn event loop to convert SystemEvents to signal updates
        let mut event_rx = manager.take_event_receiver();
        spawn_local(async move {
            while let Some(event) = event_rx.next().await {
                match event {
                    SystemEvent::StateChanged { state: new_state } => {
                        set_state.set(new_state);
                    }
                    SystemEvent::StatusUpdate { message } => {
                        set_status.set(message);
                    }
                    SystemEvent::DataReceived { data, timestamp_us } => {
                        // Forward to worker
                        if let Some(worker) = worker_signal.get_untracked() {
                            let msg =
                                crate::protocol::UiToWorker::IngestData { data, timestamp_us };
                            send_to_worker(&worker, msg, set_status);
                        }
                    }
                    SystemEvent::ProbeProgress { baud, message } => {
                        set_detected_baud.set(baud);
                        set_status.set(message);
                    }
                    SystemEvent::RxActivity => {
                        set_rx_active.set(true);
                        spawn_local(async move {
                            gloo_timers::future::sleep(std::time::Duration::from_millis(100)).await;
                            set_rx_active.set(false);
                        });
                    }
                    SystemEvent::TxActivity => {
                        set_tx_active.set(true);
                        spawn_local(async move {
                            gloo_timers::future::sleep(std::time::Duration::from_millis(100)).await;
                            set_tx_active.set(false);
                        });
                    }
                    SystemEvent::Error { message } => {
                        #[cfg(debug_assertions)]
                        web_sys::console::error_1(&format!("Actor error: {}", message).into());
                        set_status.set(format!("Error: {}", message));
                    }
                    SystemEvent::DecoderChanged { id } => {
                        set_decoder_id.set(id.clone());

                        // Forward to worker
                        if let Some(worker) = worker_signal.get_untracked() {
                            let msg = crate::protocol::UiToWorker::SetDecoder { id };
                            send_to_worker(&worker, msg, set_status);
                        }
                    }
                }
            }
        });

        Self {
            manager,
            state,
            set_state,
            status,
            set_status,
            detected_baud,
            set_detected_baud,
            detected_framing,
            set_detected_framing,
            rx_active,
            set_rx_active,
            tx_active,
            set_tx_active,
            decoder_id,
            set_decoder_id,
            framer_id,
            set_framer_id,
            worker_signal,
            #[cfg(target_arch = "wasm32")]
            last_port: std::rc::Rc::new(std::cell::RefCell::new(None)),
        }
    }

    // ═══════════════════════════════════════════════════════════════
    // Public API: UI → Actors
    // ═══════════════════════════════════════════════════════════════

    /// Request connection to a serial port
    pub async fn connect(&self, port: web_sys::SerialPort, baud: u32, framing: &str) {
        // Extract port info from SerialPort
        let info = port.get_info();
        let vid = js_sys::Reflect::get(&info, &"usbVendorId".into())
            .ok()
            .and_then(|v| v.as_f64())
            .map(|v| v as u16);
        let pid = js_sys::Reflect::get(&info, &"usbProductId".into())
            .ok()
            .and_then(|v| v.as_f64())
            .map(|v| v as u16);

        let port_info = SerialPortInfo {
            path: format!("{:04X}:{:04X}", vid.unwrap_or(0), pid.unwrap_or(0)),
            vid,
            pid,
        };

        // Store the port for reconfigure and pass through command instead of global state
        #[cfg(target_arch = "wasm32")]
        {
            *self.last_port.borrow_mut() = Some(port.clone());
            let _ = self.manager.send_command_with_port(
                UiCommand::Connect {
                    port: port_info,
                    baud,
                    framing: framing.to_string(),
                },
                port,
            );
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            let _ = self.manager.send_command(UiCommand::Connect {
                port: port_info,
                baud,
                framing: framing.to_string(),
            });
        }

        // Send config to worker
        self.send_worker_config(baud);
    }

    /// Request disconnection
    pub async fn disconnect(&self) {
        let _ = self.manager.send_command(UiCommand::Disconnect);
    }

    /// Write data to the connected port
    pub async fn write(&self, data: &[u8]) -> Result<(), String> {
        let _ = self.manager.send_command(UiCommand::WriteData {
            data: data.to_vec(),
        });
        Ok(())
    }

    /// Change the active decoder
    pub fn set_decoder(&self, id: String) {
        self.set_decoder_id.set(id.clone());

        let _ = self
            .manager
            .send_command(UiCommand::SetDecoder { id: id.clone() });

        if let Some(worker) = self.worker_signal.get_untracked() {
            let msg = crate::protocol::UiToWorker::SetDecoder { id };
            send_to_worker(&worker, msg, self.set_status);
        }
    }

    /// Change decoder using typed enum
    pub fn set_decoder_typed(&self, decoder: core_types::DecoderId) {
        let id = match decoder {
            core_types::DecoderId::Utf8 => "utf8".to_string(),
            core_types::DecoderId::Hex => "hex".to_string(),
            core_types::DecoderId::Mavlink => "mavlink".to_string(),
        };
        self.set_decoder(id);
    }

    /// Change the active framer
    pub fn set_framer(&self, id: String) {
        self.set_framer_id.set(id.clone());

        let _ = self
            .manager
            .send_command(UiCommand::SetFramer { id: id.clone() });

        if let Some(worker) = self.worker_signal.get_untracked() {
            let msg = crate::protocol::UiToWorker::SetFramer { id };
            send_to_worker(&worker, msg, self.set_status);
        }
    }

    /// Change framer using typed enum
    pub fn set_framer_typed(&self, framer: core_types::FramerId) {
        let id = match framer {
            core_types::FramerId::Raw => "raw".to_string(),
            core_types::FramerId::Lines => "lines".to_string(),
            core_types::FramerId::Cobs => "cobs".to_string(),
            core_types::FramerId::Slip => "slip".to_string(),
        };
        self.set_framer(id);
    }

    /// Reconfigure connection parameters
    pub fn reconfigure(&self, baud: u32, framing: String) {
        // Pass port through command instead of global state
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(port) = self.last_port.borrow().as_ref() {
                let _ = self
                    .manager
                    .send_command_with_port(UiCommand::Reconfigure { baud, framing }, port.clone());
                return;
            }
        }

        // Fallback for non-WASM or if no port stored
        let _ = self
            .manager
            .send_command(UiCommand::Reconfigure { baud, framing });
    }

    /// Send worker configuration
    pub fn send_worker_config(&self, baud: u32) {
        if let Some(worker) = self.worker_signal.get_untracked() {
            let msg = crate::protocol::UiToWorker::Connect { baud_rate: baud };
            send_to_worker(&worker, msg, self.set_status);
        }
    }

    // === Port Picker Methods ===

    /// Request port from user (shows browser port picker)
    pub async fn request_port(&self) -> Option<web_sys::SerialPort> {
        let window = web_sys::window()?;
        let navigator = window.navigator();
        let serial = navigator.serial();

        let promise = serial.request_port();

        match wasm_bindgen_futures::JsFuture::from(promise).await {
            Ok(port_js) => port_js.dyn_into::<web_sys::SerialPort>().ok(),
            Err(_) => None,
        }
    }

    /// Auto-select port by VID/PID (skip picker if match found)
    /// If VID/PID is None (fresh session), auto-selects if only 1 device available
    pub async fn auto_select_port(
        &self,
        vid: Option<u16>,
        pid: Option<u16>,
    ) -> Option<web_sys::SerialPort> {
        #[cfg(debug_assertions)]
        web_sys::console::log_1(
            &format!(
                "DEBUG: auto_select_port called with VID={:04X?}, PID={:04X?}",
                vid, pid
            )
            .into(),
        );

        let window = web_sys::window()?;
        let navigator = window.navigator();
        let serial = navigator.serial();

        let ports_promise = serial.get_ports();
        let ports_js = wasm_bindgen_futures::JsFuture::from(ports_promise)
            .await
            .ok()?;
        let ports_array: js_sys::Array = ports_js.dyn_into().ok()?;

        #[cfg(debug_assertions)]
        web_sys::console::log_1(
            &format!("DEBUG: getPorts() returned {} ports", ports_array.length()).into(),
        );

        let mut exact_match: Option<web_sys::SerialPort> = None;
        let mut fallback_port: Option<web_sys::SerialPort> = None;
        let mut port_count = 0;

        for i in 0..ports_array.length() {
            let port_js = ports_array.get(i);
            if let Ok(port) = port_js.dyn_into::<web_sys::SerialPort>() {
                let info = port.get_info();
                let port_vid = js_sys::Reflect::get(&info, &"usbVendorId".into())
                    .ok()
                    .and_then(|v| v.as_f64())
                    .map(|v| v as u16);
                let port_pid = js_sys::Reflect::get(&info, &"usbProductId".into())
                    .ok()
                    .and_then(|v| v.as_f64())
                    .map(|v| v as u16);

                #[cfg(debug_assertions)]
                web_sys::console::log_1(
                    &format!(
                        "DEBUG: Port {}: VID={:04X?}, PID={:04X?}",
                        i, port_vid, port_pid
                    )
                    .into(),
                );

                // Check for exact match (if VID/PID was provided)
                if let (Some(target_vid), Some(target_pid)) = (vid, pid) {
                    if port_vid == Some(target_vid) && port_pid == Some(target_pid) {
                        #[cfg(debug_assertions)]
                        web_sys::console::log_1(&"DEBUG: Exact match found!".into());
                        exact_match = Some(port);
                        break;
                    }
                }

                // Count real USB devices (ignore virtual COM ports)
                if port_vid.is_some() && port_pid.is_some() {
                    port_count += 1;
                    fallback_port = Some(port);
                }
            }
        }

        // Return exact match if found
        if let Some(port) = exact_match {
            return Some(port);
        }

        // Fallback: If only 1 real USB device available, use it
        // (Handles fresh session or device swap)
        if port_count == 1 {
            #[cfg(debug_assertions)]
            if vid.is_none() {
                web_sys::console::log_1(
                    &"DEBUG: Fresh session (no stored device), auto-selecting single USB device"
                        .into(),
                );
            } else {
                web_sys::console::log_1(
                    &"DEBUG: No exact match, but only 1 USB device available. Using it as fallback"
                        .into(),
                );
            }

            if fallback_port.is_some() {
                #[cfg(debug_assertions)]
                web_sys::console::log_1(&"DEBUG: Returning fallback port".into());
                return fallback_port;
            } else {
                #[cfg(debug_assertions)]
                web_sys::console::log_1(&"DEBUG: ERROR - fallback_port is None!".into());
            }
        }

        #[cfg(debug_assertions)]
        web_sys::console::log_1(
            &format!(
                "DEBUG: {} USB devices available, showing picker (fallback_port={:?})",
                port_count,
                fallback_port.is_some()
            )
            .into(),
        );

        None
    }

    // === Auto-Reconnect (handled by ReconnectActor) ===

    /// Setup auto-reconnect (compatibility method - actors handle this automatically)
    #[allow(clippy::too_many_arguments)] // API compatibility with ConnectionManager
    pub fn setup_auto_reconnect(
        &self,
        _last_vid: Signal<Option<u16>>,
        _last_pid: Signal<Option<u16>>,
        _set_last_vid: WriteSignal<Option<u16>>,
        _set_last_pid: WriteSignal<Option<u16>>,
        _baud: Signal<u32>,
        _detected_baud: ReadSignal<u32>,
        _framing: Signal<String>,
    ) {
        // ReconnectActor handles this automatically via USB events
        // This method exists only for API compatibility with ConnectionManager
    }

    /// Clear auto-reconnect device
    pub fn clear_auto_reconnect_device(&self) {
        // Handled by ReconnectActor
    }

    // === Activity Indicators ===

    /// Trigger RX activity indicator
    pub fn trigger_rx(&self) {
        self.set_rx_active.set(true);
    }

    /// Trigger TX activity indicator
    pub fn trigger_tx(&self) {
        self.set_tx_active.set(true);
    }

    // ═══════════════════════════════════════════════════════════════
    // Public API: Reactive State (Actors → UI)
    // ═══════════════════════════════════════════════════════════════

    /// Get current status message (reactive)
    pub fn get_status(&self) -> Signal<String> {
        self.status.into()
    }

    /// Get current connection state (reactive)
    pub fn state(&self) -> ReadSignal<ConnectionState> {
        self.state
    }

    /// Get current status message (reactive)
    pub fn status(&self) -> ReadSignal<String> {
        self.status
    }

    /// Get RX activity indicator (reactive)
    pub fn rx_active(&self) -> ReadSignal<bool> {
        self.rx_active
    }

    /// Get TX activity indicator (reactive)
    pub fn tx_active(&self) -> ReadSignal<bool> {
        self.tx_active
    }

    /// Derived signal: can the user disconnect?
    pub fn can_disconnect(&self) -> Signal<bool> {
        let state = self.state;
        Signal::derive(move || state.get().can_disconnect())
    }

    /// Derived signal: should the button show "Disconnect"?
    pub fn button_shows_disconnect(&self) -> Signal<bool> {
        let state = self.state;
        Signal::derive(move || state.get().button_shows_disconnect())
    }

    /// Derived signal: indicator color
    pub fn indicator_color(&self) -> Signal<&'static str> {
        let state = self.state;
        Signal::derive(move || state.get().indicator_color())
    }

    /// Derived signal: should indicator pulse?
    pub fn indicator_should_pulse(&self) -> Signal<bool> {
        let state = self.state;
        Signal::derive(move || state.get().indicator_should_pulse())
    }
}

#[cfg(test)]
mod tests {
    // Note: These tests can only run in a browser environment with Leptos runtime
    // For now, we'll rely on integration tests in Phase 4
}
