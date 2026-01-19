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


    // Hooks for external UI updates (optional, or we just expose signals)

    // RX/TX Activity Signals
    pub rx_active: Signal<bool>,
    set_rx_active: WriteSignal<bool>,
    pub tx_active: Signal<bool>,
    set_tx_active: WriteSignal<bool>,
}

impl ConnectionManager {
    pub fn new(worker_signal: Signal<Option<Worker>>) -> Self {
        let (connected, set_connected) = create_signal(false);
        let (status, set_status) = create_signal("Ready to connect".to_string());
        let (is_reconfiguring, set_is_reconfiguring) = create_signal(false);
        let (detected_baud, set_detected_baud) = create_signal(0);
        let (detected_framing, set_detected_framing) = create_signal("".to_string());

        let (rx_active, set_rx_active) = create_signal(false);
        let (tx_active, set_tx_active) = create_signal(false);

        Self {
            connected: connected.into(),
            set_connected,
            status: status.into(),
            set_status,
            is_reconfiguring: is_reconfiguring.into(),
            set_is_reconfiguring,
            detected_baud: detected_baud.into(),
            set_detected_baud,
            detected_framing: detected_framing.into(),
            set_detected_framing,
            transport: Rc::new(RefCell::new(None)),
            active_port: Rc::new(RefCell::new(None)),
            worker: worker_signal,
            last_auto_baud: Rc::new(RefCell::new(None)),
            read_loop_should_stop: Rc::new(RefCell::new(false)),

            rx_active: rx_active.into(),
            set_rx_active,
            tx_active: tx_active.into(),
            set_tx_active,
        }
    }

    pub fn trigger_rx(&self) {
        if !self.rx_active.get_untracked() {
            self.set_rx_active.set(true);
            let s = self.set_rx_active;
            // Blink for 100ms
            spawn_local(async move {
                let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(&mut |r, _| {
                    let _ = web_sys::window()
                        .unwrap()
                        .set_timeout_with_callback_and_timeout_and_arguments_0(&r, 100);
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
                    let _ = web_sys::window()
                        .unwrap()
                        .set_timeout_with_callback_and_timeout_and_arguments_0(&r, 70);
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
        if baud > 0 && framing == "Auto" {
            let (detect_framing, initial_buf) = self.smart_probe_framing(port.clone(), baud).await;
            self.connect_impl(port, baud, &detect_framing, Some(initial_buf)).await
        } else {
            // Auto-Detect if Baud is 0
            let (final_baud, final_framing_str, initial_buffer) = if baud == 0 {
                let (b, f, buf) = self.detect_config(port.clone(), framing).await;
                (b, f, Some(buf))
            } else {
                (baud, framing.to_string(), None)
            };
    
            self.connect_impl(port, final_baud, &final_framing_str, initial_buffer)
                .await
        }
    }

    pub async fn disconnect(&self) {
        self.set_status.set("Disconnecting...".into());

        // 1. Signal Read Loop to Stop
        *self.read_loop_should_stop.borrow_mut() = true;

        // 2. Wait for it to exit (e.g. 200ms)
        // This gives the read loop a chance to see the flag and break, dropping its borrow of transport.
        let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(&mut |r, _| {
            let _ = web_sys::window()
                .unwrap()
                .set_timeout_with_callback_and_timeout_and_arguments_0(&r, 200);
        }))
        .await;

        // 3. Close Transport
        // Try to take lock. If still locked, we warn but don't panic.
        let mut t_opt = None;
        if let Ok(mut borrow) = self.transport.try_borrow_mut() {
            t_opt = borrow.take();
        } else {
             web_sys::console::warn_1(&"Disconnect: Could not acquire transport lock even after wait.".into());
        }

        if let Some(mut t) = t_opt {
            let _ = t.close().await;
        }

        // 2. Clear Port
        *self.active_port.borrow_mut() = None;

        self.set_connected.set(false);
        self.set_status.set("Ready to connect".into());
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
                let _ = web_sys::window()
                    .unwrap()
                    .set_timeout_with_callback_and_timeout_and_arguments_0(&r, 200);
            }))
            .await;

            // 3. Close existing
            let mut t_opt = None;
             if let Ok(mut borrow) = self.transport.try_borrow_mut() {
                t_opt = borrow.take();
            } else {
                 web_sys::console::warn_1(&"Reconfigure: Could not acquire transport lock even after wait.".into());
            }

            if let Some(mut t) = t_opt {
                // Resize safe close
                let _ = t.close().await;
            }

            // 2. Wait for browser to release lock fully
            let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(&mut |r, _| {
                let _ = web_sys::window()
                    .unwrap()
                    .set_timeout_with_callback_and_timeout_and_arguments_0(&r, 100);
            }))
            .await;

            // 3. Open New
            // We reuse the connect_impl logic, manually handling detection so we can check for cancellation
            let (final_baud, final_framing, initial_buf) = if baud == 0 {
                let cached_auto = *self.last_auto_baud.borrow();

                // IMPROVEMENT: If we have a trusted PAST auto-detection result, use it.
                // This persists even if user temporarily switches to a wrong Manual baud rate.
                // This also avoids the aggressive 'enter' probe on context switch.
                if let Some(cached) = cached_auto {
                    let effective_framing = if framing == "Auto" {
                        "8N1"
                    } else {
                        framing
                    };
                    (cached, effective_framing.to_string(), None)
                } else {
                    let (b, f, buf) = self.detect_config(port.clone(), framing).await;
                    // RACE CHECK: If disconnected during detection, abort
                    if self.active_port.borrow().is_none() {
                        self.set_is_reconfiguring.set(false);
                        return;
                    }
                    (b, f, Some(buf))
                }
            } else if framing == "Auto" {
                 // Smart Probe on Reconfigure as well
                 let (detect_f, buf) = self.smart_probe_framing(port.clone(), baud).await;
                 (baud, detect_f, Some(buf))
            } else {
                (baud, framing.to_string(), None)
            };

            // Final sanity check before opening
            if self.active_port.borrow().is_none() {
                self.set_is_reconfiguring.set(false);
                return;
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
                            let _ = web_sys::window()
                                .unwrap()
                                .set_timeout_with_callback_and_timeout_and_arguments_0(&r, 10);
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
    pub async fn auto_select_port(
        &self,
        last_vid: Option<u16>,
        last_pid: Option<u16>,
    ) -> Option<web_sys::SerialPort> {
        let nav = web_sys::window().unwrap().navigator();
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
        use wasm_bindgen::JsValue; // Added this line to make JsValue available

        let nav = web_sys::window().unwrap().navigator();
        let serial = nav.serial();

        if serial.is_undefined() {
            self.set_status
                .set("Error: WebSerial not supported.".into());
            return None;
        }

        let options = js_sys::Object::new();
        // Common USB-Serial VIDs (Filtered)
        let vids = vec![
            0x0403, 0x10C4, 0x1A86, 0x067B, 0x303A, 0x2341, 0x239A, 0x0483, 0x1366, 0x2E8A, 0x03EB,
            0x1FC9, 0x0D28,
        ];
        let filters = js_sys::Array::new();
        for vid in vids {
            let f = js_sys::Object::new();
            let _ = js_sys::Reflect::set(&f, &"usbVendorId".into(), &JsValue::from(vid));
            filters.push(&f);
        }
        let _ = js_sys::Reflect::set(&options, &"filters".into(), &filters);

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
        let d = chars[0].to_digit(10).unwrap_or(8) as u8;
        let p = match chars[1] {
            'N' => "none",
            'E' => "even",
            'O' => "odd",
            _ => "none",
        }
        .to_string();
        let s_bits = chars[2].to_digit(10).unwrap_or(1) as u8;
        (d, p, s_bits)
    }

    pub async fn detect_config(
        &self,
        port: web_sys::SerialPort,
        current_framing: &str,
    ) -> (u32, String, Vec<u8>) {
        let baud_candidates = vec![
            115200, 1500000, 9600, 921600, 460800, 230400, 57600, 38400, 19200,
        ];
        let mut best_score = 0.0;
        let mut best_rate = 115200;
        let mut best_framing = "8N1".to_string();
        let mut best_buffer = Vec::new();

        // Helpers
        // Local scoring logic removed in favor of `analysis` crate.

        'outer: for rate in baud_candidates {
            self.set_status.set(format!("Scanning {}...", rate));
            web_sys::console::log_1(&format!("AUTO: Probing [v2] {}...", rate).into());
            let probe_start_ts = js_sys::Date::now();

            // 1. Probe 8N1
                let buffer = self.gather_probe_data(port.clone(), rate, "8N1", true).await;
                let open_dur = js_sys::Date::now() - probe_start_ts;
                web_sys::console::log_1(
                    &format!("PROFILE: Rate {} PROBED in {:.1}ms. Bytes: {}", rate, open_dur, buffer.len()).into(),
                );

            if buffer.is_empty() {
                continue;
            }

            // 2. Analyze
            let score_8n1 = analysis::calculate_score_8n1(&buffer);
            let score_7e1 = analysis::calculate_score_7e1(&buffer);

            web_sys::console::log_1(
                &format!(
                    "AUTO: Rate {} => 8N1 Score: {:.4} (Size: {}), 7E1 Score: {:.4}",
                    rate,
                    score_8n1,
                    buffer.len(),
                    score_7e1
                )
                .into(),
            );

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

            if best_score > 0.95 {
                *self.last_auto_baud.borrow_mut() = Some(best_rate); // SAVE CACHE
                break 'outer;
            }

            // 3. Fallback: Deep Probe if Auto Framing
            if current_framing == "Auto" && best_score < 0.5 {
                for fr in ["8E1", "8O1"] {
                    self.set_status
                        .set(format!("Deep Probe {} {}...", rate, fr));
                    
                    let buf2 = self.gather_probe_data(port.clone(), rate, fr, true).await; // Use helper
                    
                    /* Original Deep Probe Logic Removed - replaced by helper call */
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
        (best_rate, best_framing, best_buffer)
    }

    // Helper: Gather Probe Data (Extracted)
    async fn gather_probe_data(&self, port: web_sys::SerialPort, rate: u32, framing: &str, send_wakeup: bool) -> Vec<u8> {
         let mut t = WebSerialTransport::new();
         let (d,p,s) = Self::parse_framing(framing);
         let cfg = SerialConfig {
            baud_rate: rate,
            data_bits: d,
            parity: p,
            stop_bits: s,
            flow_control: "none".into(),
        };

        let mut buffer = Vec::new();
        if t.open(port, cfg).await.is_ok() {
            if send_wakeup {
                let _ = t.write(b"\r").await;
            }
            
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
                             // Use analysis crate if available or simple check (Assuming analysis crate in scope)
                             if analysis::calculate_score_8n1(&buffer) > 0.90 {
                                 break;
                             }
                        }
                     }
                }
                let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(&mut |r, _| {
                    let _ = web_sys::window().unwrap().set_timeout_with_callback_and_timeout_and_arguments_0(&r, 10);
                })).await;
            }
            let _ = t.close().await;
        }
        buffer
    }

    // New: Smart Probe for Single Baud
    pub async fn smart_probe_framing(&self, port: web_sys::SerialPort, rate: u32) -> (String, Vec<u8>) {
        self.set_status.set(format!("Smart Probing {}...", rate));
        
        // 1. Probe 8N1 (Most common) - PASSIVE (No Wakeup)
        let buf_8n1 = self.gather_probe_data(port.clone(), rate, "8N1", false).await;
        if !buf_8n1.is_empty() {
             let score = analysis::calculate_score_8n1(&buf_8n1);
             if score > 0.90 {
                 return ("8N1".to_string(), buf_8n1);
             }
        }

        // 2. Probe 7E1 (Common alternative) - PASSIVE
        if !buf_8n1.is_empty() { // Only if we saw SOME data (garbage or not), try parity
             let buf_7e1 = self.gather_probe_data(port.clone(), rate, "7E1", false).await;
             let score = analysis::calculate_score_7e1(&buf_7e1); // Assuming 7E1 score calc
             if score > 0.90 {
                 return ("7E1".to_string(), buf_7e1);
             }
        }
        
        // Default to 8N1 if silent or unsure
        ("8N1".to_string(), buf_8n1)
    }
    // Internal connect implementation
    async fn connect_impl(
        &self,
        port: web_sys::SerialPort,
        baud: u32,
        framing: &str,
        initial_buffer: Option<Vec<u8>>,
    ) -> Result<(), String> {
        let (d, p, s) = Self::parse_framing(framing);

        let cfg = SerialConfig {
            baud_rate: baud,
            data_bits: d,
            parity: p,
            stop_bits: s,
            flow_control: "none".into(),
        };

        let mut t = WebSerialTransport::new();
        match t.open(port.clone(), cfg).await {
            Ok(_) => {
                // Reset stop signal
                *self.read_loop_should_stop.borrow_mut() = false;
                
                // Store state
                *self.transport.borrow_mut() = Some(t);
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
                        if let Some(w) = self.worker.get_untracked() {
                            let msg = UiToWorker::IngestData {
                                data: buf,
                                timestamp_us: (js_sys::Date::now() * 1000.0) as u64,
                            };
                            if let Ok(cmd_val) = serde_wasm_bindgen::to_value(&msg) {
                                let _ = w.post_message(&cmd_val);
                            }
                        }
                    }
                }

                Ok(())
            }
            Err(e) => {
                self.set_status.set(format!("Connection Failed: {:?}", e));
                Err(format!("{:?}", e))
            }
        }
    }

    pub fn set_decoder(&self, id: String) {
        if let Some(w) = self.worker.get_untracked() {
            let msg = UiToWorker::SetDecoder { id };
            if let Ok(cmd_val) = serde_wasm_bindgen::to_value(&msg) {
                let _ = w.post_message(&cmd_val);
            }
        }
    }

    pub fn set_framer(&self, id: String) {
        if let Some(w) = self.worker.get_untracked() {
            let msg = UiToWorker::SetFramer { id };
            if let Ok(cmd_val) = serde_wasm_bindgen::to_value(&msg) {
                let _ = w.post_message(&cmd_val);
            }
        }
    }

    // --- Auto-Reconnect Logic ---
    pub fn setup_auto_reconnect(
        &self,
        last_vid: Signal<Option<u16>>,
        last_pid: Signal<Option<u16>>,
        baud_signal: Signal<u32>,
        detected_baud: Signal<u32>,
        framing_signal: Signal<String>,
    ) {
        let manager_conn = self.clone();
        let manager_disc = self.clone();
        let is_reconfiguring = self.is_reconfiguring;

        let on_connect_closure = Closure::wrap(Box::new(move |_e: web_sys::Event| {
            // On Connect (Device plugged in)
            // Check if it matches our last device
            if let (Some(target_vid), Some(target_pid)) =
                (last_vid.get_untracked(), last_pid.get_untracked())
            {
                let manager_conn = manager_conn.clone();
                spawn_local(async move {
                    let nav = web_sys::window().unwrap().navigator();
                    let serial = nav.serial();
                    // getPorts() returns Promise directly
                    let promise = serial.get_ports();

                    if let Ok(val) = wasm_bindgen_futures::JsFuture::from(promise).await {
                        let ports: js_sys::Array = val.unchecked_into();
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

                            if vid == Some(target_vid) && pid == Some(target_pid) {
                                manager_conn
                                    .set_status
                                    .set("Device found. Auto-reconnecting...".into());

                                // We reuse the `options` / `baud`
                                // Use default framing for auto-reconnect (or derived from valid config)
                                let user_pref_baud = baud_signal.get_untracked();
                                let last_known_baud = detected_baud.get_untracked();

                                // SMART RECONNECT:
                                // If user meant "Auto" (0), but we successfully connected before (last_known > 0),
                                // reuse that rate to avoid a full re-scan (which sends '\r' and takes time).
                                let target_baud = if user_pref_baud == 0 && last_known_baud > 0 {
                                    last_known_baud
                                } else if user_pref_baud == 0 {
                                    115200 // Fallback if no history (shouldn't happen on reconnect usually)
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
                                // Auto-reconnect not needed here, handled by manager internal state or explicit loop
                                web_sys::console::log_1(
                                    &format!(
                                        "DEBUG: Auto-Connect. Pref: {}, Last: {}, Target: {}",
                                        user_pref_baud, last_known_baud, target_baud
                                    )
                                    .into(),
                                );

                                spawn_local(async move {
                                    // FORCE RESET: Close any stale handles (even if we think we are disconnected, the browser might hold the lock)
                                    manager_conn.disconnect().await;

                                    // Wait for OS/Browser to release resource fully (Crucial for auto-reconnect)
                                    let _ = wasm_bindgen_futures::JsFuture::from(
                                            js_sys::Promise::new(&mut |r, _| {
                                                let _ = web_sys::window().unwrap().set_timeout_with_callback_and_timeout_and_arguments_0(&r, 500);
                                            })
                                        ).await;

                                    // Manager updates status signals automatically
                                    if let Err(_e) = manager_conn
                                        .connect(p, target_baud, &final_framing_str)
                                        .await
                                    {
                                        // Connect failed
                                    } else {
                                        // Manual status update for "Restored" vs just "Connected"
                                        manager_conn.set_status.set("Restored Connection".into());
                                    }
                                });
                                return; // Stop checking
                            }
                        }
                    }
                });
            }
        }) as Box<dyn FnMut(_)>);

        // On Disconnect
        let on_disconnect_closure = Closure::wrap(Box::new(move |_e: web_sys::Event| {
            if is_reconfiguring.get_untracked() {
                return;
            }
            manager_disc
                .set_status
                .set("Device Disconnected (Waiting to Reconnect...)".into());
            manager_disc.set_connected.set(false);
        }) as Box<dyn FnMut(_)>);

        // Attach listeners
        let nav = web_sys::window().unwrap().navigator();
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
