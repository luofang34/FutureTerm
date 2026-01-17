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

mod connection;
use connection::ConnectionManager;

mod xterm;
pub mod protocol;
pub mod worker_logic;

#[component]
pub fn App() -> impl IntoView {
    let (_terminal_ready, set_terminal_ready) = create_signal(false);
    
    // Worker Signal (Used by ConnectionManager)
    let (worker, set_worker) = create_signal::<Option<Worker>>(None);
    
    // Connection Manager encapsulates Status, Connected, Transport, Port
    let manager = ConnectionManager::new(worker.into());
    let status = manager.get_status();
    let connected = manager.get_connected();
    let is_reconfiguring = manager.is_reconfiguring;
    let detected_baud = manager.detected_baud;
    let detected_framing = manager.detected_framing;

    let (baud_rate, set_baud_rate) = create_signal(0);
    
    // Framing Signal (String "8N1", "8E1", etc.)
    let (framing, set_framing) = create_signal("Auto".to_string());
    
    // Auto-Detect Feedback Signals (Only for UI display when in Auto mode)
    // These are now part of ConnectionManager
    // let (detected_baud, set_detected_baud) = create_signal::<Option<u32>>(None);
    // let (detected_framing, set_detected_framing) = create_signal::<Option<String>>(None);
    
    // Direct Terminal Handle
    let (term_handle, set_term_handle) = create_signal::<Option<xterm::TerminalHandle>>(None);

    // Track parsed events
    let (events_list, set_events_list) = create_signal::<Vec<String>>(Vec::new());

    // Legacy signals removed/replaced by manager:
    // status, connected, transport, active_port, is_reconfiguring

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
                                 manager.set_status.set(s.clone());
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
            manager.set_status.set("Failed to spawn worker".into());
        }
    });

    // Shared Helper
    let parse_framing = |f: &str| -> (u8, String, u8) {
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

    // Transport removed
    let manager_con_main = manager.clone();
    // Use manager for disconnect
    let manager_disc = manager.clone();
    
    let on_connect = move |force_picker: bool| {
         let shift_held = force_picker;
         // Toggle Logic
         if connected.get() && !force_picker {
             // Disconnect Logic
             let manager_d = manager_disc.clone();
             spawn_local(async move {
                 manager_d.disconnect().await;
                 manager_d.set_status.set("Disconnected".into());
             });
             return;
         }
         
         // Reset detected info
         manager.set_detected_baud.set(0);
         manager.set_detected_framing.set("".into());

         let current_baud = baud_rate.get_untracked();
         
         // Store connection info for checking against future events
          // Load connection info from local storage if available
          let storage = web_sys::window().unwrap().local_storage().ok().flatten();
          let init_vid = storage.as_ref().and_then(|s| s.get_item("last_vid").ok().flatten()).and_then(|s| s.parse::<u16>().ok());
          let init_pid = storage.as_ref().and_then(|s| s.get_item("last_pid").ok().flatten()).and_then(|s| s.parse::<u16>().ok());
          
          let (last_vid, set_last_vid) = create_signal::<Option<u16>>(init_vid);
          let (last_pid, set_last_pid) = create_signal::<Option<u16>>(init_pid);
          let manager = manager_con_main.clone();

         spawn_local(async move {
             let nav = web_sys::window().unwrap().navigator();
             let serial = nav.serial();
             
             if serial.is_undefined() {
                 manager.set_status.set("Error: WebSerial not supported.".into());
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
                                    manager.set_status.set("Auto-selected known port...".into());
                                    break;
                                }
                            }
                        }
                        
                        // Fallback: If no match but only 1 port exists, use it
                        if !matched && ports.length() == 1 {
                             final_port = Some(ports.get(0).unchecked_into());
                             manager.set_status.set("Auto-selected single available port...".into());
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
                                     Err(_) => { manager.set_status.set("Cancelled".into()); return; }
                                 }
                             },
                             Err(_) => { manager.set_status.set("Error: requestPort call failed".into()); return; }
                         }
                     },
                     Err(_) => { manager.set_status.set("Error: requestPort not found".into()); return; }
                 }
             }
             
             if let Some(port) = final_port {
                             
                             // Hot-Swap: If already connected, close the old connection first!
                             // Hot-Swap: If already connected, close the old connection first!
                            if manager.connected.get_untracked() {
                                manager.set_status.set("Switching Port...".into());
                                manager.disconnect().await;
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

                             

                             let current_framing = framing.get_untracked();

                             let mut final_baud = current_baud;
                             let mut final_framing_str = if current_framing == "Auto" { "8N1".to_string() } else { current_framing.clone() };

                             if final_baud == 0 || current_framing == "Auto" {
                                 manager.set_status.set("Auto-Detecting Config...".into());
                                 
                                     
                             web_sys::console::log_1(&format!("Smart Port Check: VID={:?} PID={:?}", vid, pid).into());
                             
                             let manager_conn = manager.clone();
                             spawn_local(async move {
                                // Manager handles detection if baud == 0
                                match manager_conn.connect(port, current_baud, &current_framing).await {
                                     Ok(_) => {
                                         // Save to LocalStorage
                                        if let (Some(v), Some(p)) = (vid, pid) {
                                            if let Ok(Some(storage)) = web_sys::window().unwrap().local_storage() {
                                                let _ = storage.set_item("last_vid", &v.to_string());
                                                let _ = storage.set_item("last_pid", &p.to_string());
                                            }
                                        }
                                     },
                                     Err(_) => {
                                         // Status updated by manager
                                     }
                                }
                             });

                                     // --- Auto-Reconnect Listeners ---
                                     // We set these up ONCE per successful requestPort session
                                     // Since `requestPort` gives us permission, we can scan later.
                                     
                                      let manager_conn_cl = manager.clone();
                                     let manager_conn = manager_conn_cl.clone();
                                     let on_connect_closure = Closure::wrap(Box::new(move |_e: web_sys::Event| {
                                         // On Connect (Device plugged in)
                                         // Check if it matches our last device
                                         if let (Some(target_vid), Some(target_pid)) = (last_vid.get_untracked(), last_pid.get_untracked()) {
                                               let manager_conn = manager_conn_cl.clone();
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
                                                                manager_conn.set_status.set("Device found. Auto-reconnecting...".into());
                                                                
                                                                // We reuse the `options` / `baud`
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
                                                                 // Manager Connect (Handles open, loop, worker)
                                                                 let manager_conn = manager_conn.clone();
                                                                 spawn_local(async move {
                                                                    // Manager updates status signals automatically
                                                                    if let Err(_e) = manager_conn.connect(p, if current_baud == 0 { 115200 } else { current_baud }, &final_framing_str).await {
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
                                          if is_reconfiguring.get_untracked() { return; }
                                          manager.set_status.set("Device Disconnected (Waiting to Reconnect...)".into());
                                          manager.set_connected.set(false);
                                     }) as Box<dyn FnMut(_)>);
                                     serial.set_ondisconnect(Some(on_disconnect.as_ref().unchecked_ref()));
                                     on_disconnect.forget();


                                }
                                }

          });
    };
    
    // --- Dynamic Reconfiguration Effect ---
    let manager_reconf = manager.clone();
    
    create_effect(move |_| {
         let b = baud_rate.get();
         let f = framing.get();
         
         // Only reconfigure if already connected (Untracked to avoid triggering on connect)
         if connected.get_untracked() {
             let manager_r = manager_reconf.clone();
             
             spawn_local(async move {
                 // If b=0 and f=Auto, we assume it's the "Auto" state and don't force reconfig 
                 // (unless we add a "Re-Scan" button later, but for now this prevents redundant loops if both set to Auto)
                 // Allow Auto (0 / Auto) to trigger reconfiguration too
                 
                 web_sys::console::log_1(&"Dynamically Reconfiguring Port...".into());
                 
                 // Manager Reconfigure (Handles Close -> Open -> Loop)
                 // Pass `b` and `f` directly. If b=0, Manager detects. If f=Auto, Manager probes.
                 manager_r.reconfigure(b, &f).await;
             });
         }
    });

    let on_connect_arrow = on_connect.clone();
    let manager_tx = manager.clone();

    
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
                    <option value="0" selected=move || baud_rate.get() == 0>
                        {move || if baud_rate.get() == 0 && detected_baud.get() > 0 {
                            format!("Auto ({})", detected_baud.get())
                        } else {
                            "Auto Baudrate".to_string()
                        }}
                    </option>
                    <option value="9600">9600</option>
                    <option value="115200">115200</option>
                    <option value="1000000">1000000</option>
                    <option value="1500000">1500000</option>
                </select>

                <select 
                    style="background: #333; color: white; border: 1px solid #555; padding: 4px; border-radius: 4px; margin-left: 10px;"
                     on:change=move |ev| {
                          set_framing.set(event_target_value(&ev));
                     }
                     prop:value=move || framing.get()>
                    <option value="Auto" selected=move || framing.get() == "Auto">
                        {move || if framing.get() == "Auto" && !detected_framing.get().is_empty() {
                            format!("Auto ({})", detected_framing.get())
                        } else {
                            "Auto Parity".to_string()
                        }}
                    </option>
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
                            let manager_tx = manager_tx.clone();
                            let on_data_cb = Closure::wrap(Box::new(move |data: JsValue| {
                                if let Some(text) = data.as_string() {
                                    let bytes = text.into_bytes();
                                    
                                    // Direct TX on Main Thread
                                    let active_manager = manager_tx.clone();
                                    spawn_local(async move {
                                        if let Err(e) = active_manager.write(&bytes).await {
                                            web_sys::console::log_1(&format!("TX Error: {:?}", e).into());
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

pub fn parse_framing(s: &str) -> (u8, String, u8) {
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

