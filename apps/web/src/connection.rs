use leptos::*;
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

use core_types::{SerialConfig, Transport};
use transport_webserial::WebSerialTransport;
use wasm_bindgen_futures::spawn_local;
use web_sys::Worker;

// We need to move the protocol module usage here or make it public
use crate::protocol::UiToWorker;

#[derive(Clone)]
pub struct ConnectionManager {
    // State Signals
    pub connected: Signal<bool>,
    pub set_connected: WriteSignal<bool>,
    pub status: Signal<String>,
    pub set_status: WriteSignal<String>,
    pub is_reconfiguring: Signal<bool>,
    pub set_is_reconfiguring: WriteSignal<bool>,
    pub is_probing: Signal<bool>,
    pub set_is_probing: WriteSignal<bool>,

    // Detected Config (for UI feedback)
    pub detected_baud: Signal<u32>,
    pub set_detected_baud: WriteSignal<u32>,
    pub detected_framing: Signal<String>,
    pub set_detected_framing: WriteSignal<String>,

    // Internal State
    transport: Rc<RefCell<Option<WebSerialTransport>>>,
    active_port: Rc<RefCell<Option<web_sys::SerialPort>>>,
    worker: Signal<Option<Worker>>, // Read-only access to worker for sending data
    last_auto_baud: Rc<RefCell<Option<u32>>>,
    // Signal to stop the read loop gracefully
    read_loop_should_stop: Rc<RefCell<bool>>,
    // Guard against double-connect race conditions
    is_connecting_internal: Rc<RefCell<bool>>,
    // Guard against duplicate disconnect calls during auto-reconnect
    is_disconnecting: Rc<RefCell<bool>>,

    // Hooks for external UI updates (optional, or we just expose signals)

    // RX/TX Activity Signals
    pub rx_active: Signal<bool>,
    set_rx_active: WriteSignal<bool>,
    pub tx_active: Signal<bool>,
    set_tx_active: WriteSignal<bool>,

    // Decoder State
    pub decoder_id: Signal<String>,
    pub set_decoder_id: WriteSignal<String>,

    // User-initiated disconnect flag (to distinguish from device loss)
    user_initiated_disconnect: Rc<RefCell<bool>>,
}

impl ConnectionManager {
    pub fn new(worker_signal: Signal<Option<Worker>>) -> Self {
        let (connected, set_connected) = create_signal(false);
        let (status, set_status) = create_signal("Ready to connect".to_string());
        let (is_reconfiguring, set_is_reconfiguring) = create_signal(false);
        let (is_probing, set_is_probing) = create_signal(false);
        let (detected_baud, set_detected_baud) = create_signal(0);
        let (detected_framing, set_detected_framing) = create_signal("".to_string());

        let (rx_active, set_rx_active) = create_signal(false);
        let (tx_active, set_tx_active) = create_signal(false);
        let (decoder_id, set_decoder_id) = create_signal("utf8".to_string());

        Self {
            connected: connected.into(),
            set_connected,
            status: status.into(),
            set_status,
            is_reconfiguring: is_reconfiguring.into(),
            set_is_reconfiguring,
            is_probing: is_probing.into(),
            set_is_probing,
            detected_baud: detected_baud.into(),
            set_detected_baud,
            detected_framing: detected_framing.into(),
            set_detected_framing,
            transport: Rc::new(RefCell::new(None)),
            active_port: Rc::new(RefCell::new(None)),
            worker: worker_signal,
            last_auto_baud: Rc::new(RefCell::new(None)),
            read_loop_should_stop: Rc::new(RefCell::new(false)),
            is_connecting_internal: Rc::new(RefCell::new(false)),
            is_disconnecting: Rc::new(RefCell::new(false)),

            rx_active: rx_active.into(),
            set_rx_active,
            tx_active: tx_active.into(),
            set_tx_active,
            decoder_id: decoder_id.into(),
            set_decoder_id,
            user_initiated_disconnect: Rc::new(RefCell::new(false)),
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

    pub fn get_status(&self) -> Signal<String> {
        self.status
    }

    pub fn get_connected(&self) -> Signal<bool> {
        self.connected
    }

    // Async Connect
    pub async fn connect(
        &self,
        port: web_sys::SerialPort,
        baud: u32,
        framing: &str,
    ) -> Result<(), String> {
        if *self.is_connecting_internal.borrow() {
            // OPTIMIZATION: Log as info instead of warn (expected during auto-reconnect races)
            web_sys::console::log_1(&"Connection attempt blocked: Already connecting.".into());
            return Err("Already connecting".to_string());
        }

        // Stale UI Guard
        if self.active_port.borrow().is_some() {
            web_sys::console::warn_1(&"Connection attempt blocked: Port already active.".into());
            return Err("Already connected".to_string());
        }

        *self.is_connecting_internal.borrow_mut() = true;

        let result = if baud > 0 && framing == "Auto" {
            let (detect_framing, initial_buf, proto) =
                self.smart_probe_framing(port.clone(), baud).await;

            if let Some(p) = proto {
                self.set_decoder(p);
            }

            self.connect_impl(port, baud, &detect_framing, Some(initial_buf))
                .await
        } else {
            // Auto-Detect if Baud is 0
            let (final_baud, final_framing_str, initial_buffer, detected_proto) = if baud == 0 {
                let (b, f, buf, proto) = self.detect_config(port.clone(), framing).await;

                // CRITICAL FIX: Wait for OS to fully release port after probing
                // Probing opens/closes port multiple times - OS needs time to clean up
                web_sys::console::log_1(
                    &"DEBUG: Probing complete, waiting 150ms for port cleanup".into(),
                );
                let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(&mut |r, _| {
                    if let Some(window) = web_sys::window() {
                        let _ =
                            window.set_timeout_with_callback_and_timeout_and_arguments_0(&r, 150);
                    }
                }))
                .await;

                (b, f, Some(buf), proto)
            } else {
                (baud, framing.to_string(), None, None)
            };

            // Switch Decoder if detected
            if let Some(p) = detected_proto {
                self.set_decoder(p);
            }

            self.connect_impl(port, final_baud, &final_framing_str, initial_buffer)
                .await
        };

        *self.is_connecting_internal.borrow_mut() = false;
        result
    }

    pub async fn disconnect(&self) -> bool {
        // CRITICAL FIX: Prevent duplicate disconnect() calls during auto-reconnect races
        // When multiple auto-reconnect tasks spawn, they may both call disconnect() simultaneously
        // This guard ensures only ONE disconnect runs, preventing transport lock conflicts
        {
            let mut flag = self.is_disconnecting.borrow_mut();
            if *flag {
                web_sys::console::log_1(
                    &"DEBUG: disconnect() - already in progress, skipping".into(),
                );
                return false; // Another disconnect() is already running, caller should exit
            }
            *flag = true; // Claim the disconnect slot
        }

        let t0 = js_sys::Date::now();
        web_sys::console::log_1(&format!("[{}ms] disconnect() - START", t0 as u64).into());

        // Mark this as user-initiated disconnect
        *self.user_initiated_disconnect.borrow_mut() = true;

        self.set_status.set("Disconnecting...".into());

        // OPTIMIZATION: Detect if device is already physically disconnected
        // If so, we can skip close() and save ~150ms
        let already_disconnected = !self.connected.get_untracked();
        if already_disconnected {
            web_sys::console::log_1(
                &format!(
                    "[{}ms] disconnect() - device already disconnected, skipping close()",
                    (js_sys::Date::now() - t0) as u64
                )
                .into(),
            );
        }

        // 1. Signal Read Loop to Stop
        *self.read_loop_should_stop.borrow_mut() = true;
        web_sys::console::log_1(&"DEBUG: disconnect() - signaled read loop to stop".into());

        // 2. Wait for read loop to exit (with retry logic)
        // CRITICAL FIX: Wait longer (300ms) to ensure read loop completes async read_chunk()
        // Read loop may be blocked in t.read_chunk().await which holds immutable borrow
        // If we don't wait long enough, auto-reconnect's connect() will fail to acquire lock
        let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(&mut |r, _| {
            if let Some(window) = web_sys::window() {
                let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&r, 300);
            }
        }))
        .await;
        web_sys::console::log_1(&"DEBUG: disconnect() - waited 300ms for read loop".into());

        // 3. Close Transport with retry logic
        // OPTIMIZATION: Skip close() if device already disconnected (saves ~150ms)
        if already_disconnected {
            // Device physically disconnected - just clear the transport reference
            // CRITICAL: Retry to ensure read loop has released borrow
            let mut cleared = false;
            for attempt in 1..=10 {
                if let Ok(mut borrow) = self.transport.try_borrow_mut() {
                    *borrow = None;
                    cleared = true;
                    if attempt > 1 {
                        web_sys::console::log_1(
                            &format!(
                                "DEBUG: disconnect() - cleared transport on attempt {} (device \
                                 already disconnected)",
                                attempt
                            )
                            .into(),
                        );
                    } else {
                        web_sys::console::log_1(
                            &"DEBUG: disconnect() - cleared transport (device already \
                              disconnected)"
                                .into(),
                        );
                    }
                    break;
                }
                // Wait 50ms before retry (total max 500ms)
                let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(&mut |r, _| {
                    if let Some(window) = web_sys::window() {
                        let _ =
                            window.set_timeout_with_callback_and_timeout_and_arguments_0(&r, 50);
                    }
                }))
                .await;
            }
            if !cleared {
                // CRITICAL FIX: Force close the port to release browser lock
                // We've waited 800ms for read loop to exit gracefully, but it's still holding the
                // borrow Closing the port will force read_chunk() to error and read
                // loop to exit
                web_sys::console::warn_1(
                    &"CRITICAL: Forcing port close after failed transport cleanup (device \
                      disconnected)"
                        .into(),
                );

                // Try to access active_port and close it
                // This should cause read loop's read_chunk() to fail and release the borrow
                if let Some(port) = self.active_port.borrow().as_ref() {
                    let promise = port.close();
                    web_sys::console::log_1(&"DEBUG: Forced port.close() called".into());

                    // CRITICAL: WAIT for close() to complete before continuing
                    // This ensures read loop has exited before we return
                    let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
                    web_sys::console::log_1(&"DEBUG: Forced port.close() completed".into());

                    // Try one last time to clear transport after port is closed
                    if let Ok(mut borrow) = self.transport.try_borrow_mut() {
                        *borrow = None;
                        web_sys::console::log_1(
                            &"DEBUG: Transport cleared after forced port close".into(),
                        );
                    } else {
                        web_sys::console::warn_1(
                            &"WARNING: Transport still locked even after forced port close".into(),
                        );
                    }
                } else {
                    web_sys::console::warn_1(
                        &"WARNING: Could not access active_port for forced close".into(),
                    );
                }
            }
        } else {
            // Normal disconnect - need to close transport properly
            let mut t_opt = None;
            let mut retry_count = 0;
            let max_retries = 5;

            loop {
                if let Ok(mut borrow) = self.transport.try_borrow_mut() {
                    t_opt = borrow.take();
                    web_sys::console::log_1(
                        &format!(
                            "DEBUG: disconnect() - acquired transport lock (attempt {})",
                            retry_count + 1
                        )
                        .into(),
                    );
                    break;
                } else {
                    retry_count += 1;
                    if retry_count >= max_retries {
                        web_sys::console::warn_1(
                            &format!(
                                "Disconnect: Could not acquire transport lock after {} retries \
                                 ({}ms total)",
                                max_retries,
                                max_retries * 300
                            )
                            .into(),
                        );
                        break;
                    }
                    // Wait 300ms before retry
                    let _ =
                        wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(&mut |r, _| {
                            if let Some(window) = web_sys::window() {
                                let _ = window
                                    .set_timeout_with_callback_and_timeout_and_arguments_0(&r, 300);
                            }
                        }))
                        .await;
                    web_sys::console::log_1(
                        &format!(
                            "DEBUG: disconnect() - retry {} acquiring transport lock",
                            retry_count
                        )
                        .into(),
                    );
                }
            }

            if let Some(mut t) = t_opt {
                web_sys::console::log_1(&"DEBUG: disconnect() - calling transport.close()".into());
                let _ = t.close().await;
                web_sys::console::log_1(&"DEBUG: disconnect() - transport.close() returned".into());
            } else {
                web_sys::console::log_1(
                    &"DEBUG: disconnect() - no transport to close (already None)".into(),
                );
            }
        }

        // 2. Clear Port
        *self.active_port.borrow_mut() = None;

        self.set_connected.set(false);
        self.set_status.set("Ready to connect".into());

        let elapsed = (js_sys::Date::now() - t0) as u64;
        web_sys::console::log_1(
            &format!(
                "[{}ms] disconnect() - COMPLETE (took {}ms)",
                elapsed, elapsed
            )
            .into(),
        );

        // Reset the disconnect guard flag
        *self.is_disconnecting.borrow_mut() = false;

        true // Return true to indicate disconnect completed successfully
    }

    // Reconfigure = Disconnect + Connect (Atomic logic)
    pub async fn reconfigure(&self, baud: u32, framing: &str) {
        if !self.connected.get_untracked() {
            return;
        }

        // Set flag to suppress "Device Lost" logic in read loop (if it races)
        self.set_is_reconfiguring.set(true);
        self.set_status.set("Reconfiguring...".into());

        let port_opt = self.active_port.borrow().clone();

        if let Some(port) = port_opt {
            // 1. Signal Read Loop to Stop
            *self.read_loop_should_stop.borrow_mut() = true;

            // 2. Wait for it to exit (e.g. 200ms)
            let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(&mut |r, _| {
                if let Some(window) = web_sys::window() {
                    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&r, 200);
                }
            }))
            .await;

            // 3. Close existing
            let mut t_opt = None;
            if let Ok(mut borrow) = self.transport.try_borrow_mut() {
                t_opt = borrow.take();
            } else {
                web_sys::console::warn_1(
                    &"Reconfigure: Could not acquire transport lock even after wait.".into(),
                );
            }

            if let Some(mut t) = t_opt {
                // Resize safe close
                let _ = t.close().await;
            }

            // 2. Wait for browser to release lock fully
            let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(&mut |r, _| {
                if let Some(window) = web_sys::window() {
                    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&r, 100);
                }
            }))
            .await;

            // 3. Open New
            // We reuse the connect_impl logic, manually handling detection so we can check for
            // cancellation
            let (final_baud, final_framing, initial_buf, proto) = if baud == 0 {
                let cached_auto = *self.last_auto_baud.borrow();

                // IMPROVEMENT: If we have a trusted PAST auto-detection result, use it.
                // This persists even if user temporarily switches to a wrong Manual baud rate.
                // This also avoids the aggressive 'enter' probe on context switch.
                if let Some(cached) = cached_auto {
                    let effective_framing = if framing == "Auto" { "8N1" } else { framing };
                    (cached, effective_framing.to_string(), None, None)
                } else {
                    let (b, f, buf, proto) = self.detect_config(port.clone(), framing).await;
                    // RACE CHECK: If disconnected during detection, abort
                    if self.active_port.borrow().is_none() {
                        self.set_is_reconfiguring.set(false);
                        return;
                    }
                    (b, f, Some(buf), proto)
                }
            } else if framing == "Auto" {
                // Smart Probe on Reconfigure as well
                let (detect_f, buf, proto) = self.smart_probe_framing(port.clone(), baud).await;
                if let Some(p) = proto.clone() {
                    self.set_decoder(p);
                }
                (baud, detect_f, Some(buf), proto)
            } else {
                (baud, framing.to_string(), None, None)
            };

            // Final sanity check before opening
            if self.active_port.borrow().is_none() {
                self.set_is_reconfiguring.set(false);
                return;
            }

            if let Some(p) = proto {
                self.set_decoder(p);
            }

            match self
                .connect_impl(port, final_baud, &final_framing, initial_buf)
                .await
            {
                Ok(_) => {
                    // Success
                }
                Err(e) => {
                    self.set_status.set(format!("Reconfig Failed: {}", e));
                    self.set_connected.set(false);
                }
            }
        }

        self.set_is_reconfiguring.set(false);
    }

    #[allow(clippy::await_holding_refcell_ref)]
    fn spawn_read_loop(&self) {
        let t_strong = self.transport.clone();
        let connected_signal = self.connected;
        let set_connected = self.set_connected;
        let set_status = self.set_status;
        let is_reconf = self.is_reconfiguring;
        let worker_signal = self.worker;

        let manager = self.clone();

        let should_stop = self.read_loop_should_stop.clone();

        spawn_local(async move {
            web_sys::console::log_1(&"DEBUG: Read Loop STARTED".into());
            loop {
                // Check stop signal FIRST
                if *should_stop.borrow() {
                    web_sys::console::log_1(&"DEBUG: Read Loop STOP SIGNAL received.".into());
                    break;
                }

                let mut chunk = Vec::new();
                let mut ts = 0;
                let mut should_break = false;

                // Scope to drop borrow
                {
                    if let Ok(borrow) = t_strong.try_borrow() {
                        if let Some(t) = borrow.as_ref() {
                            if !t.is_open() {
                                web_sys::console::log_1(
                                    &"DEBUG: Read Loop - Transport Closed".into(),
                                );
                                should_break = true;
                            } else {
                                match t.read_chunk().await {
                                    Ok((d, t_val)) => {
                                        chunk = d;
                                        ts = t_val;
                                    }
                                    Err(e) => {
                                        web_sys::console::log_1(
                                            &format!("DEBUG: Read Loop - Read Error: {:?}", e)
                                                .into(),
                                        );
                                        should_break = true;
                                    }
                                }
                            }
                        } else {
                            web_sys::console::log_1(&"DEBUG: Read Loop - Transport None".into());
                            should_break = true;
                        }
                    } else {
                        web_sys::console::log_1(&"DEBUG: Read Loop - Transport Locked".into());
                        should_break = true;
                    }
                }

                if should_break {
                    break;
                }

                if !chunk.is_empty() {
                    if let Some(w) = worker_signal.get_untracked() {
                        let msg = UiToWorker::IngestData {
                            data: chunk,
                            timestamp_us: ts,
                        };
                        if let Ok(cmd_val) = serde_wasm_bindgen::to_value(&msg) {
                            let _ = w.post_message(&cmd_val);
                        }
                    }
                    // Trigger RX
                    manager.trigger_rx();
                } else {
                    // Yield
                    let _ =
                        wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(&mut |r, _| {
                            if let Some(window) = web_sys::window() {
                                let _ = window
                                    .set_timeout_with_callback_and_timeout_and_arguments_0(&r, 10);
                            }
                        }))
                        .await;
                }
            }

            web_sys::console::log_1(&"DEBUG: Read Loop EXITED".into());

            // Loop exited
            if connected_signal.get_untracked() && !is_reconf.get_untracked() {
                set_status.set("Device Lost".into());
                set_connected.set(false);
            }
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
    #[allow(clippy::await_holding_refcell_ref)]
    pub async fn write(&self, data: &[u8]) -> Result<(), String> {
        if let Ok(borrow) = self.transport.try_borrow() {
            if let Some(t) = borrow.as_ref() {
                if t.is_open() {
                    if let Err(e) = t.write(data).await {
                        return Err(format!("TX Error: {:?}", e));
                    }
                    self.trigger_tx();
                    return Ok(());
                }
            }
        }
        Err("TX Dropped: Transport busy/locked or closed".to_string())
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
        // CRITICAL: Set probing flag to ignore disconnect events during auto-detection
        self.set_is_probing.set(true);

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

        // Clear probing flag
        self.set_is_probing.set(false);

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
                    // Reset stop signal
                    *self.read_loop_should_stop.borrow_mut() = false;

                    // CRITICAL FIX: Store state with retry logic to avoid RefCell panic
                    // If previous disconnect() couldn't acquire lock, old read loop may still hold
                    // borrow
                    let mut transport_opt = Some(t);
                    let mut stored = false;

                    for attempt in 1..=5 {
                        if let Ok(mut borrow) = self.transport.try_borrow_mut() {
                            *borrow = transport_opt.take();
                            stored = true;
                            if attempt > 1 {
                                web_sys::console::log_1(
                                    &format!(
                                        "DEBUG: connect() - stored transport on attempt {}",
                                        attempt
                                    )
                                    .into(),
                                );
                            }
                            break;
                        }
                        // Wait 200ms before retry
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
                        web_sys::console::log_1(
                            &format!(
                                "DEBUG: connect() - retry {} acquiring transport lock",
                                attempt
                            )
                            .into(),
                        );
                    }

                    if !stored {
                        web_sys::console::error_1(
                            &"CRITICAL: connect() could not store transport after retries".into(),
                        );
                        break Err("Could not acquire transport lock".into());
                    }

                    *self.active_port.borrow_mut() = Some(port);

                    self.set_connected.set(true);
                    self.set_status.set("Connected".into());

                    // Update detected config for UI
                    self.set_detected_baud.set(baud);
                    self.set_detected_framing.set(framing.to_string());

                    // Spawn Read Loop
                    self.spawn_read_loop();

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
        let manager_conn = self.clone();
        let manager_disc = self.clone();
        let is_reconfiguring = self.is_reconfiguring;
        let is_probing = self.is_probing;

        let on_connect_closure = Closure::wrap(Box::new(move |_e: web_sys::Event| {
            let t_onconnect = js_sys::Date::now();
            web_sys::console::log_1(
                &format!("[{}ms] serial.onconnect triggered", t_onconnect as u64).into(),
            );

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
                                manager_conn
                                    .set_status
                                    .set("Device found. Auto-reconnecting...".into());

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

                                // CRITICAL FIX: Check if reconnection already in progress to
                                // prevent race When multiple
                                // serial.onconnect events fire (wrong device, then correct device),
                                // both the immediate check and retry loop can trigger reconnection
                                // simultaneously
                                if *manager_conn.is_connecting_internal.borrow() {
                                    web_sys::console::log_1(
                                        &"DEBUG: Skipping auto-reconnect - connection already in \
                                          progress"
                                            .into(),
                                    );
                                    return; // Exit early, don't spawn duplicate reconnect
                                }

                                spawn_local(async move {
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
                                    *manager_conn.user_initiated_disconnect.borrow_mut() = false;
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
                                        // Manual status update for "Restored" vs just "Connected"
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
                        // 10s, serial.onconnect fallback still applies.
                        for retry_attempt in 1..=200 {
                            // OPTIMIZATION: Exit retry loop if another path already started
                            // reconnecting This happens when a new
                            // serial.onconnect event finds device while retry loop is still running
                            if *manager_conn.is_connecting_internal.borrow() {
                                web_sys::console::log_1(
                                    &format!(
                                        "[{}ms] Retry loop - exiting early, reconnection already \
                                         started by another path",
                                        (js_sys::Date::now() - t0) as u64
                                    )
                                    .into(),
                                );
                                return;
                            }

                            // Wait 50ms before retry
                            let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(
                                &mut |r, _| {
                                    if let Some(window) = web_sys::window() {
                                        let _ = window
                                            .set_timeout_with_callback_and_timeout_and_arguments_0(
                                                &r, 50,
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
                                        manager_conn
                                            .set_status
                                            .set("Device found. Auto-reconnecting...".into());

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

                                        // CRITICAL FIX: Check if reconnection already in progress
                                        // to prevent race
                                        // Retry loop may find device after immediate check already
                                        // triggered reconnect
                                        if *manager_conn.is_connecting_internal.borrow() {
                                            web_sys::console::log_1(
                                                &"DEBUG: Retry loop - skipping reconnect, \
                                                  connection already in progress"
                                                    .into(),
                                            );
                                            return; // Exit retry loop, don't spawn duplicate
                                                    // reconnect
                                        }

                                        let manager_conn_clone = manager_conn.clone();
                                        spawn_local(async move {
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

                                            *manager_conn_clone
                                                .user_initiated_disconnect
                                                .borrow_mut() = false;
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

            if is_reconfiguring.get_untracked() {
                web_sys::console::log_1(&"DEBUG: ignoring disconnect (reconfiguring)".into());
                return;
            }

            if is_probing.get_untracked() {
                web_sys::console::log_1(&"DEBUG: ignoring disconnect (probing)".into());
                return;
            }

            // Check if this was a user-initiated disconnect
            let user_initiated = *manager_disc.user_initiated_disconnect.borrow();

            if user_initiated {
                web_sys::console::log_1(
                    &"DEBUG: User-initiated disconnect - disabling auto-reconnect".into(),
                );
                // Clear last_vid/last_pid to prevent auto-reconnect
                set_last_vid.set(None);
                set_last_pid.set(None);
                manager_disc.set_status.set("Disconnected".into());
                // Reset the flag for next time
                *manager_disc.user_initiated_disconnect.borrow_mut() = false;
            } else {
                web_sys::console::log_1(
                    &"DEBUG: Device disconnected (not user-initiated) - will auto-reconnect".into(),
                );
                // Keep last_vid/last_pid to enable auto-reconnect
                manager_disc
                    .set_status
                    .set("Device Lost (Auto-reconnecting...)".into());
            }

            manager_disc.set_connected.set(false);
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

            // Leak closures to keep them alive
            on_connect_closure.forget();
            on_disconnect_closure.forget();
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
}
