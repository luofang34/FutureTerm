use leptos::*;
use std::rc::Rc;
use std::cell::RefCell;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Worker, MessageEvent};
use transport_webserial::WebSerialTransport;
use core_types::{Transport, SerialConfig};
use wasm_bindgen_futures::spawn_local;

// We need to move the protocol module usage here or make it public
use crate::protocol::{UiToWorker, WorkerToUi};


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
    
    // Hooks for external UI updates (optional, or we just expose signals)
}

impl ConnectionManager {
    pub fn new(worker_signal: Signal<Option<Worker>>) -> Self {
        let (connected, set_connected) = create_signal(false);
        let (status, set_status) = create_signal("Ready to connect".to_string());
        let (is_reconfiguring, set_is_reconfiguring) = create_signal(false);
        let (detected_baud, set_detected_baud) = create_signal(0);
        let (detected_framing, set_detected_framing) = create_signal("".to_string());
        
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
        }
    }
    
    pub fn get_status(&self) -> Signal<String> {
        self.status
    }

    pub fn get_connected(&self) -> Signal<bool> {
        self.connected
    }
    
    // Async Connect
    pub async fn connect(&self, port: web_sys::SerialPort, baud: u32, framing: &str) -> Result<(), String> {
        // Auto-Detect if Baud is 0
        let (final_baud, final_framing_str, initial_buffer) = if baud == 0 {
             let (b, f, buf) = self.detect_config(port.clone(), framing).await;
             (b, f, Some(buf))
        } else {
             (baud, framing.to_string(), None)
        };
        
        self.connect_impl(port, final_baud, &final_framing_str, initial_buffer).await
    }
    
    pub async fn disconnect(&self) {
        self.set_status.set("Disconnecting...".into());
        
        // 1. Close Transport (Retry loop to avoid panic)
        let mut t_opt = None;
        for _ in 0..100 {
            if let Ok(mut borrow) = self.transport.try_borrow_mut() {
                t_opt = borrow.take();
                break;
            }
            // Wait 20ms
            let _ = wasm_bindgen_futures::JsFuture::from(
                js_sys::Promise::new(&mut |r, _| {
                     let _ = web_sys::window().unwrap().set_timeout_with_callback_and_timeout_and_arguments_0(&r, 20);
                })
            ).await;
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
        if !self.connected.get_untracked() { return; }
        
        // Set flag to suppress "Device Lost" logic in read loop (if it races)
        self.set_is_reconfiguring.set(true);
        self.set_status.set("Reconfiguring...".into());
        
        let port_opt = self.active_port.borrow().clone();
        
        if let Some(port) = port_opt {
            // 1. Close existing (Internal logic only, distinct from full disconnect)
             let mut t_opt = None;
             // Retry loop to acquire transport (to avoid panic if read_loop holds lock)
             for _ in 0..100 {
                 if let Ok(mut borrow) = self.transport.try_borrow_mut() {
                     t_opt = borrow.take();
                     break;
                 }
                 // Wait 20ms
                 let _ = wasm_bindgen_futures::JsFuture::from(
                    js_sys::Promise::new(&mut |r, _| {
                         let _ = web_sys::window().unwrap().set_timeout_with_callback_and_timeout_and_arguments_0(&r, 20);
                    })
                ).await;
             }
             if let Some(mut t) = t_opt {
                 // Retry loop to close safely
                 for _ in 0..10 {
                     if let Ok(_) = t.close().await { break; }
                     // Wait 50ms
                     let _ = wasm_bindgen_futures::JsFuture::from(
                        js_sys::Promise::new(&mut |r, _| {
                             let _ = web_sys::window().unwrap().set_timeout_with_callback_and_timeout_and_arguments_0(&r, 50);
                        })
                   ).await;
                 }
             }
             
             // 2. Wait for browser to release lock fully
              let _ = wasm_bindgen_futures::JsFuture::from(
                    js_sys::Promise::new(&mut |r, _| {
                         let _ = web_sys::window().unwrap().set_timeout_with_callback_and_timeout_and_arguments_0(&r, 100);
                    })
               ).await;
             
             // 3. Open New
             // We reuse the connect_impl logic, manually handling detection so we can check for cancellation
             let (final_baud, final_framing, initial_buf) = if baud == 0 {
                 let (b, f, buf) = self.detect_config(port.clone(), framing).await;
                 // RACE CHECK: If disconnected during detection, abort
                 if self.active_port.borrow().is_none() {
                     self.set_is_reconfiguring.set(false);
                     return;
                 }
                 (b, f, Some(buf))
             } else {
                 (baud, framing.to_string(), None)
             };

             // Final sanity check before opening
             if self.active_port.borrow().is_none() { 
                 self.set_is_reconfiguring.set(false);
                 return; 
             }

             match self.connect_impl(port, final_baud, &final_framing, initial_buf).await {
                 Ok(_) => {
                     // Success
                 },
                 Err(e) => {
                     self.set_status.set(format!("Reconfig Failed: {}", e));
                     self.set_connected.set(false);
                 }
             }
        }
        
        self.set_is_reconfiguring.set(false);
    }

    fn spawn_read_loop(&self) {
        let t_strong = self.transport.clone();
        let connected_signal = self.connected;
        let set_connected = self.set_connected;
        let set_status = self.set_status;
        let is_reconf = self.is_reconfiguring;
        let worker_signal = self.worker;

        spawn_local(async move {
            loop {
                 let mut chunk = Vec::new();
                 let mut ts = 0;
                 let mut should_break = false;
                 
                 // Scope to drop borrow
                 {
                     if let Ok(borrow) = t_strong.try_borrow() {
                         if let Some(t) = borrow.as_ref() {
                             if !t.is_open() {
                                 should_break = true;
                             } else {
                                 match t.read_chunk().await {
                                     Ok((d, t_val)) => { chunk = d; ts = t_val; },
                                      Err(_) => { should_break = true; }
                                 }
                             }
                         } else { should_break = true; }
                     } else { should_break = true; }
                 }
                 
                 if should_break { break; }
                 
                 if !chunk.is_empty() {
                      if let Some(w) = worker_signal.get_untracked() {
                           let msg = UiToWorker::IngestData { data: chunk, timestamp_us: ts };
                           if let Ok(cmd_val) = serde_wasm_bindgen::to_value(&msg) {
                               let envelope = js_sys::Object::new();
                               let _ = js_sys::Reflect::set(&envelope, &"cmd".into(), &cmd_val);
                               let _ = w.post_message(&envelope);
                           }
                      }
                 } else {
                      // Yield
                      let _ = wasm_bindgen_futures::JsFuture::from(
                            js_sys::Promise::new(&mut |r, _| {
                                 let _ = web_sys::window().unwrap().set_timeout_with_callback_and_timeout_and_arguments_0(&r, 10);
                            })
                       ).await;
                 }
            }
            
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
                 let envelope = js_sys::Object::new();
                 let _ = js_sys::Reflect::set(&envelope, &"cmd".into(), &cmd_val);
                 let _ = w.post_message(&envelope);
             }
        }
    }
    pub async fn write(&self, data: &[u8]) -> Result<(), String> {
        if let Ok(borrow) = self.transport.try_borrow() {
             if let Some(t) = borrow.as_ref() {
                 if t.is_open() {
                      if let Err(e) = t.write(data).await {
                          return Err(format!("TX Error: {:?}", e));
                      }
                      return Ok(());
                 }
             }
         }
         Err("TX Dropped: Transport busy/locked or closed".to_string())
    }
    // Helper: Parse Framing String
    fn parse_framing(&self, s: &str) -> (u8, String, u8) {
        let chars: Vec<char> = s.chars().collect();
        let d = chars[0].to_digit(10).unwrap_or(8) as u8;
        let p = match chars[1] {
            'N' => "none",
            'E' => "even",
            'O' => "odd",
            _ => "none",
        }.to_string();
        let s_bits = chars[2].to_digit(10).unwrap_or(1) as u8;
        (d, p, s_bits)
    }

    pub async fn detect_config(&self, port: web_sys::SerialPort, current_framing: &str) -> (u32, String, Vec<u8>) {
        let baud_candidates = vec![115200, 9600, 1500000, 921600, 460800, 230400, 57600, 38400, 19200];
        let mut best_score = 0.0;
        let mut best_rate = 115200;
        let mut best_framing = "8N1".to_string();
        let mut best_buffer = Vec::new();

        // Helpers
        let calculate_score = |buf: &[u8]| -> f32 {
             let mut p_count = 0;
             let mut total = 0;
             for &b in buf {
                 total += 1;
                 let c = b as char;
                 if c.is_ascii_graphic() || c == ' ' || c == '\r' || c == '\n' { p_count += 1; }
             }
             if total == 0 { 0.0 } else { p_count as f32 / total as f32 }
        };

        let check_7e1 = |buf: &[u8]| -> f32 {
             let mut p_count = 0;
             let mut total = 0;
             for &b in buf {
                 total += 1;
                 let data = b & 0x7F;
                 let received_parity = (b & 0x80) >> 7;
                 let ones = data.count_ones();
                 let expected_parity = if ones % 2 == 0 { 0 } else { 1 };
                 
                 if received_parity == expected_parity {
                     let c = data as char;
                     if c.is_ascii_graphic() || c == ' ' || c == '\r' || c == '\n' { p_count += 1; }
                 }
             }
             if total == 0 { 0.0 } else { p_count as f32 / total as f32 }
        };

        'outer: for rate in baud_candidates {
            self.set_status.set(format!("Scanning {}...", rate));
            web_sys::console::log_1(&format!("AUTO: Probing {}...", rate).into());
            
            // 1. Probe 8N1
            let mut t = WebSerialTransport::new();
            let cfg = SerialConfig {
                baud_rate: rate,
                data_bits: 8,
                parity: "none".to_string(),
                stop_bits: 1,
                flow_control: "none".into(),
            };

            let mut buffer = Vec::new();
            if let Ok(_) = t.open(port.clone(), cfg).await {
                // FLUSH: Read once to clear OS buffer garbage from previous rate
                let _ = t.read_chunk().await;
                
                let _ = t.write(b"\r").await;
                let start = js_sys::Date::now();
                while js_sys::Date::now() - start < 250.0 {
                    if let Ok((chunk, _)) = t.read_chunk().await {
                         if !chunk.is_empty() { buffer.extend_from_slice(&chunk); }
                    }
                    // Wait 10ms
                     let _ = wasm_bindgen_futures::JsFuture::from(
                        js_sys::Promise::new(&mut |r, _| {
                             let _ = web_sys::window().unwrap().set_timeout_with_callback_and_timeout_and_arguments_0(&r, 10);
                        })
                    ).await;
                }
                let _ = t.close().await;
            }
            
            if buffer.is_empty() { continue; }

            // 2. Analyze
            let score_8n1 = calculate_score(&buffer);
            let score_7e1 = check_7e1(&buffer);
            
            web_sys::console::log_1(&format!("AUTO: Rate {} => 8N1 Score: {:.4} (Size: {}), 7E1 Score: {:.4}", rate, score_8n1, buffer.len(), score_7e1).into());

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

            if best_score > 0.95 { break 'outer; }
            
            // 3. Fallback: Deep Probe if Auto Framing
            if current_framing == "Auto" && best_score < 0.5 {
                for fr in ["8E1", "8O1"] {
                     self.set_status.set(format!("Deep Probe {} {}...", rate, fr));
                     let mut t2 = WebSerialTransport::new();
                     let (d, p, s) = self.parse_framing(fr);
                     let cfg_deep = SerialConfig {
                         baud_rate: rate,
                         data_bits: d,
                         parity: p,
                         stop_bits: s,
                         flow_control: "none".into(),
                     };
                     
                     if let Ok(_) = t2.open(port.clone(), cfg_deep).await {
                         let _ = t2.write(b"\r").await;
                         let mut buf2 = Vec::new();
                         let start = js_sys::Date::now();
                         while js_sys::Date::now() - start < 100.0 {
                             if let Ok((chunk, _)) = t2.read_chunk().await {
                                 if !chunk.is_empty() { buf2.extend_from_slice(&chunk); }
                             }
                             let _ = wasm_bindgen_futures::JsFuture::from(
                                js_sys::Promise::new(&mut |r, _| {
                                     let _ = web_sys::window().unwrap().set_timeout_with_callback_and_timeout_and_arguments_0(&r, 10);
                                })
                            ).await;
                         }
                         let _ = t2.close().await;
                         
                         let score = calculate_score(&buf2);
                         if score > best_score {
                             best_score = score;
                             best_rate = rate;
                             best_framing = fr.to_string();
                             best_buffer = buf2;
                         }
                         if score > 0.95 { break 'outer; }
                     }
                }
            }
        }
        
        self.set_status.set(format!("Detected: {} {} (Score: {:.2})", best_rate, best_framing, best_score));
        (best_rate, best_framing, best_buffer)
    }
    // Internal connect implementation
    async fn connect_impl(&self, port: web_sys::SerialPort, baud: u32, framing: &str, initial_buffer: Option<Vec<u8>>) -> Result<(), String> {
        let (d, p, s) = self.parse_framing(framing);

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
                              let msg = UiToWorker::IngestData { data: buf, timestamp_us: (js_sys::Date::now() * 1000.0) as u64 };
                              if let Ok(cmd_val) = serde_wasm_bindgen::to_value(&msg) {
                                  let envelope = js_sys::Object::new();
                                  let _ = js_sys::Reflect::set(&envelope, &"cmd".into(), &cmd_val);
                                  let _ = w.post_message(&envelope);
                              }
                         }
                    }
                }
                
                Ok(())
            },
            Err(e) => {
                self.set_status.set(format!("Connection Failed: {:?}", e));
                Err(format!("{:?}", e))
            }
        }
    }
}

