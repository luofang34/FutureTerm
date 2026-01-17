use leptos::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Worker, MessageEvent};
use crate::protocol::{UiToWorker, WorkerToUi};
use transport_webserial::WebSerialTransport;
use core_types::{Transport, SerialConfig};
use std::rc::Rc;
use std::cell::RefCell;
use wasm_bindgen_futures::spawn_local;

mod xterm;
pub mod protocol;
pub mod worker_logic;

#[component]
pub fn App() -> impl IntoView {
    let (_terminal_ready, set_terminal_ready) = create_signal(false);
    let (status, set_status) = create_signal("Idle".to_string());
    let (worker, set_worker) = create_signal::<Option<Worker>>(None);
    let (baud_rate, set_baud_rate) = create_signal(0);
    // Track connection state for toggle button
    let (connected, set_connected) = create_signal(false);
    
    // Framing Signal (String "8N1", "8E1", etc.)
    let (framing, set_framing) = create_signal("8N1".to_string());
    
    // Transport - Main Thread Logic
    // Store in Rc<RefCell<Option<T>>> to share with closures
    let transport = Rc::new(RefCell::new(None::<WebSerialTransport>));
    
    // Direct Terminal Handle
    let (term_handle, set_term_handle) = create_signal::<Option<xterm::TerminalHandle>>(None);

    // Track parsed events
    let (events_list, set_events_list) = create_signal::<Vec<String>>(Vec::new());

    create_effect(move |_| {
        if let Ok(w) = Worker::new("worker_bootstrap.js") {
            // Restore TextDecoder for RX to Main Thread (if we ever want to decode locally? No, worker does that)
            // But wait, worker sends BACK a 'DataBatch' with frames. 
            // We need to print raw text to terminal.
            // The worker parses frames. Does it decode text? 
            // Looking at worker_logic.rs:
            // It receives IngestData -> Frames -> Decoder.
            // It sends back DataBatch { frames, events }.
            // Frames contain raw bytes.
            // So Main Thread needs to decode bytes to string for Xterm.
            
            let decoder = web_sys::TextDecoder::new().unwrap();
            let decode_opts = js_sys::Object::new();
            let _ = js_sys::Reflect::set(&decode_opts, &"stream".into(), &JsValue::from(true));
            let opts: web_sys::TextDecodeOptions = decode_opts.unchecked_into();

            let cb = Closure::wrap(Box::new(move |e: MessageEvent| {
                 if let Ok(msg) = serde_wasm_bindgen::from_value::<WorkerToUi>(e.data()) {
                     match msg {
                         WorkerToUi::Status(s) => {
                             // Ignore "Connected" from worker if it's just config confirmation
                             if !s.contains("Worker Ready") {
                                 set_status.set(s.clone());
                             }
                         },
                         WorkerToUi::DataBatch { frames, events } => {
                             if let Some(term) = term_handle.get_untracked() {
                                 for f in frames {
                                     if !f.bytes.is_empty() {
                                        if let Ok(text) = decoder.decode_with_u8_array_and_options(&f.bytes, &opts) {
                                            let text: String = text;
                                            if !text.is_empty() {
                                                term.write(&text);
                                            }
                                        }
                                     }
                                 }
                             }
                             
                             // Update events
                             if !events.is_empty() {
                                 set_events_list.update(|list| {
                                     for evt in events {
                                         // Keep last 50 events
                                         if list.len() >= 50 {
                                             list.remove(0);
                                         }
                                         // Format event
                                         let s = format!("{}: {}", evt.protocol, evt.summary);
                                         if !s.is_empty() {
                                            list.push(s);
                                         }
                                     }
                                 });
                             }
                         },
                         WorkerToUi::AnalyzeResult { baud_rate, score } => {
                             // Received analysis from worker (if we used worker mode)
                             web_sys::console::log_1(&format!("Worker Analysis: Baud {} Score {:.2}", baud_rate, score).into());
                         }
                     }
                 }
            }) as Box<dyn FnMut(_)>);
            w.set_onmessage(Some(cb.as_ref().unchecked_ref()));
            cb.forget();
            
            set_worker.set(Some(w));
        } else {
            set_status.set("Failed to spawn worker".into());
        }
    });

    let transport_clone = transport.clone();
    let transport_term = transport.clone();
    
    let on_connect = move |force_picker: bool| {
         let shift_held = force_picker;
         // Toggle Logic
         if connected.get() && !force_picker {
             // Disconnect Logic
             let t_c = transport_clone.clone();
             spawn_local(async move {
                 // Retry loop to acquire lock (in case reading is active)
                 let mut closed = false;
                 for _ in 0..10 { // Try for 500ms
                     if let Ok(mut borrow) = t_c.try_borrow_mut() {
                         if let Some(mut t) = borrow.take() {
                             let _ = t.close().await;
                             set_status.set("Disconnected".into());
                             set_connected.set(false);
                             closed = true;
                             
                             // Notify worker to reset
                             if let Some(w) = worker.get_untracked() {
                                 let msg = UiToWorker::Disconnect;
                                 if let Ok(cmd_val) = serde_wasm_bindgen::to_value(&msg) {
                                     let envelope = js_sys::Object::new();
                                     let _ = js_sys::Reflect::set(&envelope, &"cmd".into(), &cmd_val);
                                     let _ = w.post_message(&envelope);
                                 }
                             }
                         }
                         break;
                     }
                     // Wait 50ms
                     let _ = wasm_bindgen_futures::JsFuture::from(
                          js_sys::Promise::new(&mut |r, _| {
                              let _ = web_sys::window().unwrap().set_timeout_with_callback_and_timeout_and_arguments_0(&r, 50);
                          })
                     ).await;
                 }
                 
                 if !closed {
                     set_status.set("Error: Could not close transport (Busy)".into());
                 }
             });
             return;
         }

         let current_baud = baud_rate.get_untracked();
         let t_c = transport_clone.clone();
         
         // Store connection info for checking against future events
          // Load connection info from local storage if available
          let storage = web_sys::window().unwrap().local_storage().ok().flatten();
          let init_vid = storage.as_ref().and_then(|s| s.get_item("last_vid").ok().flatten()).and_then(|s| s.parse::<u16>().ok());
          let init_pid = storage.as_ref().and_then(|s| s.get_item("last_pid").ok().flatten()).and_then(|s| s.parse::<u16>().ok());
          
          let (last_vid, set_last_vid) = create_signal::<Option<u16>>(init_vid);
          let (last_pid, set_last_pid) = create_signal::<Option<u16>>(init_pid);

         spawn_local(async move {
             let nav = web_sys::window().unwrap().navigator();
             let serial = nav.serial();
             
             if serial.is_undefined() {
                 set_status.set("Error: WebSerial not supported.".into());
                 return;
             }

             let mut final_port: Option<web_sys::SerialPort> = None;

             // 1. Smart Check
             if !shift_held {
                 if let Ok(ports_val) = wasm_bindgen_futures::JsFuture::from(serial.get_ports()).await {
                    let ports: js_sys::Array = ports_val.unchecked_into();
                    if ports.length() > 0 {
                        // Priority: Match Last VID/PID
                        let mut matched = false;
                        if let (Some(l_vid), Some(l_pid)) = (last_vid.get_untracked(), last_pid.get_untracked()) {
                            for i in 0..ports.length() {
                                let p: web_sys::SerialPort = ports.get(i).unchecked_into();
                                let info = p.get_info();
                                let vid = js_sys::Reflect::get(&info, &"usbVendorId".into()).ok().and_then(|v| v.as_f64()).map(|v| v as u16);
                                let pid = js_sys::Reflect::get(&info, &"usbProductId".into()).ok().and_then(|v| v.as_f64()).map(|v| v as u16);
                                if vid == Some(l_vid) && pid == Some(l_pid) {
                                    final_port = Some(p);
                                    matched = true;
                                    set_status.set("Auto-selected known port...".into());
                                    break;
                                }
                            }
                        }
                        
                        // Fallback: If no match but only 1 port exists, use it
                        if !matched && ports.length() == 1 {
                             final_port = Some(ports.get(0).unchecked_into());
                             set_status.set("Auto-selected single available port...".into());
                        }
                    }
                 }
             }

             // 2. Manual Request
             if final_port.is_none() {
                 let options = js_sys::Object::new();
                 // Common USB-Serial VIDs (Filtered)
                 let vids = vec![
                     0x0403, 0x10C4, 0x1A86, 0x067B, 0x303A, 0x2341, 0x239A, 0x0483, 0x1366, 0x2E8A, 0x03EB, 0x1FC9, 0x0D28 
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
                                     Ok(val) => { final_port = Some(val.unchecked_into()); },
                                     Err(_) => { set_status.set("Cancelled".into()); return; }
                                 }
                             },
                             Err(_) => { set_status.set("Error: requestPort call failed".into()); return; }
                         }
                     },
                     Err(_) => { set_status.set("Error: requestPort not found".into()); return; }
                 }
             }
             
             if let Some(port) = final_port {
                             
                             // Hot-Swap: If already connected, close the old connection first!
                             if connected.get_untracked() {
                                 set_status.set("Switching Port...".into());
                                 if let Ok(mut borrow) = t_c.try_borrow_mut() {
                                     if let Some(mut old_t) = borrow.take() {
                                          let _ = old_t.close().await;
                                     }
                                 }
                                 // Notify worker to reset (optional but good hygiene)
                                 if let Some(w) = worker.get_untracked() {
                                     let msg = UiToWorker::Disconnect;
                                     if let Ok(cmd_val) = serde_wasm_bindgen::to_value(&msg) {
                                         let envelope = js_sys::Object::new();
                                         let _ = js_sys::Reflect::set(&envelope, &"cmd".into(), &cmd_val);
                                         let _ = w.post_message(&envelope);
                                     }
                                 }
                                 set_connected.set(false);
                             }

                             // Capture VID/PID for Reconnect
                             let info = port.get_info();
                             // web-sys SerialPortInfo doesn't expose fields directly without structural casting usually?
                             // Let's rely on Reflect for safety or try methods if available.
                             // Actually web-sys 0.3.69+ exposes `usb_vendor_id` and `usb_product_id`.
                             // Let's use Reflect to be safe against version mismatch or use provided methods.
                             // Checking docs: SerialPortInfo has `usb_vendor_id` and `usb_product_id` getters.
                             
                             // NOTE: We need to enable `SerialPortInfo` in Cargo.toml (Already Done).
                             // However, let's use a small helper to extract it safely.
                             let vid = js_sys::Reflect::get(&info, &"usbVendorId".into()).ok().and_then(|v| v.as_f64()).map(|v| v as u16);
                             let pid = js_sys::Reflect::get(&info, &"usbProductId".into()).ok().and_then(|v| v.as_f64()).map(|v| v as u16);
                             
                             // Store for reconnect
                             set_last_vid.set(vid);
                             set_last_pid.set(pid);

                             set_last_vid.set(vid);
                             set_last_pid.set(pid);

                             let mut best_buffer = Vec::new(); 
                             
                             // Helper to parse framing string to Config
                             let parse_framing = |f: &str| -> (u8, String, u8) {
                                 // Format: 8N1 (Data Parity Stop)
                                 // Data: 7 or 8. Parity: N (none), E (even), O (odd). Stop: 1 or 2.
                                 let chars: Vec<char> = f.chars().collect();
                                 if chars.len() != 3 { return (8, "none".into(), 1); }
                                 
                                 let data = chars[0].to_digit(10).unwrap_or(8) as u8;
                                 let parity = match chars[1] {
                                     'N' => "none",
                                     'E' => "even",
                                     'O' => "odd",
                                     _ => "none"
                                 }.to_string();
                                 let stop = chars[2].to_digit(10).unwrap_or(1) as u8;
                                 (data, parity, stop)
                             };

                             let current_framing = framing.get();
                             let framing_candidates = if current_framing == "Auto" {
                                 vec!["8N1", "7E1", "8E1", "8O1"]
                             } else {
                                 vec![current_framing.as_str()]
                             };

                             let mut final_baud = current_baud;
                             let mut final_framing_str = if current_framing == "Auto" { "8N1".to_string() } else { current_framing.clone() };

                             if final_baud == 0 || current_framing == "Auto" {
                                 set_status.set("Auto-Detecting Config...".into());
                                 
                                 // Candidates
                                 // If Baud is fixed (non-zero) but Framing is Auto, use fixed baud.
                                 let baud_candidates = if current_baud != 0 {
                                     vec![current_baud]
                                 } else {
                                     vec![115200, 1500000, 9600, 57600, 38400, 19200, 230400, 460800, 921600]
                                 };

                                 let mut best_score = 0.0;
                                 let mut best_rate = 115200; 
                                 let mut best_framing = "8N1".to_string();

                                 'outer: for rate in baud_candidates {
                                     for fr in &framing_candidates {
                                         set_status.set(format!("Probing {} {}...", rate, fr));
                                         let mut t = WebSerialTransport::new();
                                         let (d_p, p_p, s_p) = parse_framing(fr);

                                         let cfg = SerialConfig {
                                             baud_rate: rate,
                                             data_bits: d_p,
                                             parity: p_p,
                                             stop_bits: s_p,
                                             flow_control: "none".into(),
                                         };

                                     // Attempt open
                                     if let Ok(_) = t.open(port.clone(), cfg).await {
                                          // Probe
                                          let _ = t.write(b"\r").await;
                                          
                                          // Read 200ms
                                          let mut buffer = Vec::new();
                                          let start = js_sys::Date::now();
                                          while js_sys::Date::now() - start < 200.0 {
                                              if let Ok((chunk, _)) = t.read_chunk().await {
                                                  if !chunk.is_empty() {
                                                      buffer.extend_from_slice(&chunk);
                                                  }
                                              }
                                              let _ = wasm_bindgen_futures::JsFuture::from(
                                                  js_sys::Promise::new(&mut |r, _| {
                                                       let _ = web_sys::window().unwrap().set_timeout_with_callback_and_timeout_and_arguments_0(&r, 10);
                                                  })
                                              ).await;
                                          }
                                          let _ = t.close().await;

                                          // Score
                                          if !buffer.is_empty() {
                                              if let Ok(text) = std::str::from_utf8(&buffer) {
                                                  let mut p_count = 0;
                                                  let mut total = 0;
                                                  for c in text.chars() {
                                                      total += 1;
                                                      if c.is_ascii_graphic() || c == ' ' || c == '\r' || c == '\n' { p_count += 1; }
                                                  }
                                                  if total > 0 {
                                                      let score = p_count as f32 / total as f32;
                                                      if score > best_score {
                                                          best_score = score;
                                                          best_rate = rate;
                                                          best_framing = fr.to_string();
                                                          best_buffer = buffer.clone(); 
                                                      }
                                                      if score > 0.95 { break 'outer; } 
                                                  }
                                              }
                                          }
                                     } 
                                 } 
                                 }
                                 
                                 // Apply best results
                                 final_baud = best_rate;
                                 final_framing_str = best_framing.clone();
                                 set_status.set(format!("Detected: {} {} (Score: {:.2})", best_rate, best_framing, best_score));
                                 set_baud_rate.set(best_rate); 
                                 
                                 // If "Auto" was selected, update the UI framing selector too
                                 if current_framing == "Auto" {
                                      set_framing.set(best_framing);
                                 }
                             }

                             set_status.set(format!("Connecting at {} {}...", final_baud, final_framing_str));
                             
                             // Open Transport Locally
                             let mut t = WebSerialTransport::new();
                             let (d, p, s) = parse_framing(&final_framing_str);
                             let final_cfg = SerialConfig {
                                 baud_rate: final_baud,
                                 data_bits: d,
                                 parity: p,
                                 stop_bits: s,
                                 flow_control: "none".into(),
                             };
                             match t.open(port.clone(), final_cfg).await {
                                 Ok(_) => {
                                     set_status.set("Connected".into());
                                     set_connected.set(true);
                                     
                                     // Store transport
                                     if let Ok(mut borrow) = t_c.try_borrow_mut() {
                                         *borrow = Some(t);
                                     } else {
                                         set_status.set("Error: Could not store transport".into());
                                         return;
                                      }
                                      
                                      // Log Smart Connect Decision
                                      web_sys::console::log_1(&format!("Smart Port Check: VID={:?} PID={:?}", vid, pid).into());
                                      
                                      // Save to LocalStorage
                                      if let (Some(v), Some(p)) = (vid, pid) {
                                          if let Ok(Some(storage)) = web_sys::window().unwrap().local_storage() {
                                              let _ = storage.set_item("last_vid", &v.to_string());
                                              let _ = storage.set_item("last_pid", &p.to_string());
                                          }
                                      }

                                      // Replay Best Buffer (Instant Prompt)
                                      if !best_buffer.is_empty() {
                                         if let Some(term) = term_handle.get_untracked() {
                                              let text = String::from_utf8_lossy(&best_buffer);
                                              term.write(&text);
                                         }
                                     }
                                     
                                     
                                     // Start Read Loop
                                     let t_loop = t_c.clone();
                                     spawn_local(async move {
                                         loop {
                                             let mut chunk = Vec::new();
                                             let mut ts = 0;
                                             let mut should_break = false;
                                             
                                             // 1. Borrow Shared to Read
                                             if let Ok(borrow) = t_loop.try_borrow() {
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
                                             } else {
                                                  // Failed to borrow (Disconnect is mutating?)
                                                  should_break = true; 
                                             }

                                             if should_break { break; }
                                             
                                             // 2. Send to Worker
                                             if !chunk.is_empty() {
                                                 if let Some(w) = worker.get_untracked() {
                                                     let msg = UiToWorker::IngestData { data: chunk, timestamp_us: ts };
                                                     if let Ok(cmd_val) = serde_wasm_bindgen::to_value(&msg) {
                                                         let envelope = js_sys::Object::new();
                                                         let _ = js_sys::Reflect::set(&envelope, &"cmd".into(), &cmd_val);
                                                         let _ = w.post_message(&envelope);
                                                     }
                                                 }
                                             } else {
                                                 let _ = wasm_bindgen_futures::JsFuture::from(
                                                      js_sys::Promise::new(&mut |r, _| {
                                                          let _ = web_sys::window().unwrap().set_timeout_with_callback_and_timeout_and_arguments_0(&r, 20);
                                                      })
                                                 ).await;
                                             }
                                         }
                                         // Loop End - Disconnected unexpectedly?
                                         // If we are here, and `connected` signal is still true, it means it crashed/unplugged.
                                         if connected.get_untracked() {
                                             set_status.set("Device Lost (Reconnecting...)".into());
                                             set_connected.set(false); // UI toggle off, but we keep state for auto-reconnect
                                         }
                                     });
                                     
                                     // Send initial config to worker
                                     if let Some(w) = worker.get_untracked() {
                                         let msg = UiToWorker::Connect { baud_rate: current_baud };
                                         if let Ok(cmd_val) = serde_wasm_bindgen::to_value(&msg) {
                                             let envelope = js_sys::Object::new();
                                             let _ = js_sys::Reflect::set(&envelope, &"cmd".into(), &cmd_val);
                                             let _ = w.post_message(&envelope);
                                         }
                                     }
                                     
                                     // --- Auto-Reconnect Listeners ---
                                     // We set these up ONCE per successful requestPort session
                                     // Since `requestPort` gives us permission, we can scan later.
                                     
                                     let on_connect_closure = Closure::wrap(Box::new(move |_e: web_sys::Event| {
                                         // On Connect (Device plugged in)
                                         // Check if it matches our last device
                                         if let (Some(target_vid), Some(target_pid)) = (last_vid.get_untracked(), last_pid.get_untracked()) {
                                              let t_c = t_c.clone();
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
                                                            let vid = js_sys::Reflect::get(&info, &"usbVendorId".into()).ok().and_then(|v| v.as_f64()).map(|v| v as u16);
                                                            let pid = js_sys::Reflect::get(&info, &"usbProductId".into()).ok().and_then(|v| v.as_f64()).map(|v| v as u16);
                                                            
                                                            if vid == Some(target_vid) && pid == Some(target_pid) {
                                                                set_status.set("Device found. Auto-reconnecting...".into());
                                                                
                                                                // We reuse the `options` / `baud`
                                                                let mut t = WebSerialTransport::new();
                                                                 // Use default framing for auto-reconnect (or derived from valid config)
                                                                 let current_baud = baud_rate.get_untracked();
                                                                 let current_framing = framing.get_untracked();
                                                                 let final_framing_str = if current_framing == "Auto" { "8N1".to_string() } else { current_framing };
                                                                 let (d_r, p_r, s_r) = parse_framing(&final_framing_str);
                                                                 
                                                                 let cfg = SerialConfig {
                                                                     baud_rate: if current_baud == 0 { 115200 } else { current_baud },
                                                                     data_bits: d_r,
                                                                     parity: p_r,
                                                                     stop_bits: s_r,
                                                                     flow_control: "none".into(),
                                                                 };
                                                                match t.open(p, cfg).await {
                                                                    Ok(_) => {
                                                                        set_status.set("Restored Connection".into());
                                                                        set_connected.set(true);
                                                                         if let Ok(mut borrow) = t_c.try_borrow_mut() {
                                                                             *borrow = Some(t);
                                                                         }
                                                                         
                                                                         // Notify user (Optional, or just update status)
                                                                         set_status.set("Restored Connection".into());

                                                                         // Start Read Loop (Duplicated for robustness)
                                                                         let t_loop = t_c.clone();
                                                                         spawn_local(async move {
                                                                             loop {
                                                                                 let mut chunk = Vec::new();
                                                                                 let mut ts = 0;
                                                                                 let mut should_break = false;
                                                                                 
                                                                                 if let Ok(borrow) = t_loop.try_borrow() {
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

                                                                                 if should_break { break; }
                                                                                 
                                                                                 if !chunk.is_empty() {
                                                                                     if let Some(w) = worker.get_untracked() {
                                                                                         let msg = UiToWorker::IngestData { data: chunk, timestamp_us: ts };
                                                                                         if let Ok(cmd_val) = serde_wasm_bindgen::to_value(&msg) {
                                                                                             let envelope = js_sys::Object::new();
                                                                                             let _ = js_sys::Reflect::set(&envelope, &"cmd".into(), &cmd_val);
                                                                                             let _ = w.post_message(&envelope);
                                                                                         }
                                                                                     }
                                                                                 } else {
                                                                                     let _ = wasm_bindgen_futures::JsFuture::from(
                                                                                          js_sys::Promise::new(&mut |r, _| {
                                                                                              let _ = web_sys::window().unwrap().set_timeout_with_callback_and_timeout_and_arguments_0(&r, 20);
                                                                                          })
                                                                                     ).await;
                                                                                 }
                                                                             }
                                                                             if connected.get_untracked() {
                                                                                set_status.set("Device Lost (Reconnecting...)".into());
                                                                                set_connected.set(false);
                                                                             }
                                                                         });
                                                                         
                                                                         // Re-Send configuration
                                                                         if let Some(w) = worker.get_untracked() {
                                                                             let msg = UiToWorker::Connect { baud_rate: current_baud };
                                                                             if let Ok(cmd_val) = serde_wasm_bindgen::to_value(&msg) {
                                                                                 let envelope = js_sys::Object::new();
                                                                                 let _ = js_sys::Reflect::set(&envelope, &"cmd".into(), &cmd_val);
                                                                                 let _ = w.post_message(&envelope);
                                                                             }
                                                                         }
                                                                    },
                                                                    Err(e) => set_status.set(format!("Auto-Reconnect Failed: {:?}", e)),
                                                                }
                                                                return; // Stop checking
                                                            }
                                                       }
                                                  }
                                              });
                                         }
                                     }) as Box<dyn FnMut(_)>);
                                     
                                     // Note: We need to store this closure somewhere or it dies?
                                     // `Closure::wrap` returns a JS function. We attach it.
                                     // `serial.set_onconnect(Some(func))`.
                                     // But `serial` object effectively lives in `navigator`.
                                     // We need to keep the Closure memory alive (forget it?).
                                     // If we forget it, it leaks, but that's fine for a singleton app.
                                     
                                     // Wait, `on_connect` needs to fire even if we are not connected?
                                     // This logic is nested inside `on_connect` success.
                                     // Correct. We only want to auto-reconnect if we successfully connected once.
                                     
                                     // Attach listeners
                                     serial.set_onconnect(Some(on_connect_closure.as_ref().unchecked_ref()));
                                     on_connect_closure.forget();
                                     
                                     // On Disconnect
                                     let on_disconnect = Closure::wrap(Box::new(move |_e: web_sys::Event| {
                                          set_status.set("Device Disconnected (Waiting to Reconnect...)".into());
                                          set_connected.set(false);
                                     }) as Box<dyn FnMut(_)>);
                                     serial.set_ondisconnect(Some(on_disconnect.as_ref().unchecked_ref()));
                                     on_disconnect.forget();

                                 },
                                 Err(e) => set_status.set(format!("Open Error: {:?}", e)),
                             }
             }
         });
    };

    let on_connect_arrow = on_connect.clone();
    
    view! {
        <div style="display: flex; flex-direction: column; height: 100vh; background: rgb(25, 25, 25); color: #eee;">
            <header style="padding: 10px; background: #1a1a1a; display: flex; align-items: center; gap: 20px; border-bottom: 1px solid #333;">
                <h1 style="margin: 0; font-size: 1.2rem; font-weight: 600;">FutureTerm</h1>
                <div style="flex: 1;"></div>
                
                <select 
                    style="background: #333; color: white; border: 1px solid #555; padding: 4px; border-radius: 4px;"
                    on:change=move |ev| {
                    let val = event_target_value(&ev);
                    if let Ok(b) = val.parse::<u32>() {
                        set_baud_rate.set(b);
                    }
                }
                prop:value=move || baud_rate.get().to_string()>
                    <option value="0" selected>Auto</option>
                    <option value="9600">9600</option>
                    <option value="115200">115200</option>
                    <option value="1000000">1000000</option>
                    <option value="1500000">1500000</option>
                </select>

                <select 
                    style="background: #333; color: white; border: 1px solid #555; padding: 4px; border-radius: 4px; margin-left: 10px;"
                     on:change=move |ev| {
                          set_framing.set(event_target_value(&ev));
                     }>
                    <option value="Auto" selected>Auto</option>
                    <option value="8N1">8N1</option>
                    <option value="8E1">8E1</option>
                    <option value="8O1">8O1</option>
                    <option value="7E1">7E1</option>
                </select>

                <select 
                    style="background: #333; color: white; border: 1px solid #555; padding: 4px; border-radius: 4px;"
                    on:change=move |ev| {
                    let val = event_target_value(&ev);
                     if let Some(w) = worker.get_untracked() {
                          let msg = UiToWorker::SetFramer { id: val };
                          let _ = w.post_message(&serde_wasm_bindgen::to_value(&msg).unwrap());
                     }
                }>
                    <option value="lines">Lines</option>
                    <option value="raw" selected>Raw</option>
                    <option value="cobs">COBS</option>
                    <option value="slip">SLIP</option>
                </select>

                <select 
                    style="background: #333; color: white; border: 1px solid #555; padding: 4px; border-radius: 4px;"
                    on:change=move |ev| {
                    let val = event_target_value(&ev);
                    if let Some(w) = worker.get_untracked() {
                         let msg = UiToWorker::SetDecoder { id: val };
                         let _ = w.post_message(&serde_wasm_bindgen::to_value(&msg).unwrap());
                    }
                }>
                    <option value="utf8">UTF-8</option>
                    <option value="nmea" selected>NMEA</option>
                    <option value="hex">Hex List</option>
                </select>

                <span style="font-size: 0.9rem; color: #aaa;">{move || status.get()}</span>
                
                <style>
                    {
                    ".split-btn { transition: background-color 0.2s; }
                    .split-btn:hover { background-color: #0062a3 !important; }
                    .split-btn:active { background-color: #005a96 !important; }"
                    }
                </style>
                <div style="display: flex; align-items: stretch; height: 28px; border-radius: 4px; overflow: hidden; margin-left:10px;">
                    <button 
                        class="split-btn"
                        style="padding: 0 12px; background: #007acc; color: white; border: none; cursor: pointer; font-size: 0.9rem; border-right: 1px solid rgba(255,255,255,0.2);"
                        title="Smart Connect (Auto-detects USB-Serial)"
                        on:click=move |_| on_connect(false)>
                        {move || if connected.get() { "Disconnect" } else { "Connect" }}
                    </button>
                    <button 
                         class="split-btn"
                         style="width: 26px; background: #007acc; color: white; border: none; cursor: pointer; display: flex; align-items: center; justify-content: center; padding: 0;"
                         title="Manual Port Selection..."
                         on:click=move |_| on_connect_arrow(true)>
                        <svg width="10" height="10" viewBox="0 0 16 16" fill="currentColor" style="opacity: 0.9;">
                             <path d="M8 11L3 6h10l-5 5z"/>
                        </svg>
                    </button>
                </div>
            </header>
            <main style="flex: 1; display: flex; overflow: hidden; height: 100%;">
                <div id="terminal-container" style="flex: 1; background: #191919; overflow: hidden; display: flex; flex-direction: column; min-width: 0;">
                    <xterm::TerminalView 
                        on_mount=Callback::new(move |_| set_terminal_ready.set(true)) 
                        on_terminal_ready=Callback::from(move |t: xterm::TerminalHandle| {
                            set_term_handle.set(Some(t.clone()));
                            
                            // Bind TX
                            let t_tx = transport_term.clone(); 
                            let on_data_cb = Closure::wrap(Box::new(move |data: JsValue| {
                                if let Some(text) = data.as_string() {
                                    let bytes = text.into_bytes();
                                    
                                    // Direct TX on Main Thread
                                    let t = t_tx.clone();
                                    spawn_local(async move {
                                        // Attempt to borrow transport
                                        // read_loop holds a shared borrow, so we can also take a shared borrow!
                                        if let Ok(borrow) = t.try_borrow() {
                                            if let Some(transport) = borrow.as_ref() {
                                                if transport.is_open() {
                                                     if let Err(e) = transport.write(&bytes).await {
                                                         web_sys::console::log_1(&format!("TX Error: {:?}", e).into());
                                                     }
                                                }
                                            }
                                        } else {
                                            web_sys::console::log_1(&"TX Dropped: Transport busy/locked".into());
                                        }
                                    });
                                }
                            }) as Box<dyn FnMut(JsValue)>);

                            t.on_data(on_data_cb.into_js_value().unchecked_into());
                        })
                    />
                </div>
                <div style="width: 250px; background: #222; border-left: 1px solid #444; color: #eee; overflow-y: auto; font-family: monospace; font-size: 0.8rem; padding: 5px;">
                     <div style="font-weight: bold; border-bottom: 1px solid #555; margin-bottom: 5px;">Decoded Events</div>
                     <ul>
                         <For
                             each=move || events_list.get()
                             key=|evt| evt.clone()
                             children=|evt| view! { <li>{evt}</li> }
                         />
                     </ul>
                </div>
            </main>
        </div>
    }
}
