use leptos::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Worker, MessageEvent};
use crate::protocol::WorkerToUi; // Cleanup Trigger
use wasm_bindgen_futures::spawn_local;
use core_types::{SerialConfig, DecodedEvent};
// Imports Cleaned

mod connection;
use connection::ConnectionManager;

mod xterm;
mod hex_view;
pub mod protocol;
pub mod worker_logic;

#[derive(Clone, Copy, PartialEq)]
enum ViewMode {
    Terminal,
    Hex,
}

#[component]
pub fn App() -> impl IntoView {
    let (_terminal_ready, set_terminal_ready) = create_signal(false);
    
    // Worker Signal (Used by ConnectionManager)
    let (worker, set_worker) = create_signal::<Option<Worker>>(None);
    let (view_mode, set_view_mode) = create_signal(ViewMode::Terminal);
    
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
    let (events_list, set_events_list) = create_signal::<Vec<DecodedEvent>>(Vec::new());

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
                             // Update events
                             if !events.is_empty() {
                                 set_events_list.update(|list| {
                                     list.extend(events);
                                     // Cap at 2000 events to keep memory sane
                                     if list.len() > 2000 {
                                         let split = list.len() - 2000;
                                         list.drain(0..split);
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
                 final_port = manager.auto_select_port(last_vid.get_untracked(), last_pid.get_untracked()).await;
             }

             // 2. Manual Request
             if final_port.is_none() {
                 final_port = manager.request_port().await;
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
                             // Removed unused final_framing_str variable
                             
                             // Resolve Auto to something concrete if needed, but Manager handles it now.
                             // if current_baud == 0 { final_baud = 115200; } // REMOVED (Regression Fix)
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
                                                                 // Auto-reconnect not needed here, handled by manager internal state or explicit loop
                                                                 web_sys::console::log_1(&"DEBUG: Auto-Connect Triggered. Calling disconnect/connect sequence.".into());

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
                                                                    // We pass `current_baud` directly. If it is 0 (Auto), logic in manager.connect() will trigger detection.
                                                                    if let Err(_e) = manager_conn.connect(p, current_baud, &final_framing_str).await {
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
    let manager_tx_cb = manager.clone();

    // -- Extract Callbacks for TerminalView --
    let on_terminal_mount = Callback::new(move |_| set_terminal_ready.set(true));
    
    let on_term_ready = Callback::from(move |t: xterm::TerminalHandle| {
        set_term_handle.set(Some(t.clone()));
        
        // Bind TX
        let manager_tx = manager_tx_cb.clone();
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
    });
    
    view! {
        <div style="display: flex; flex-direction: column; height: 100vh; background: rgb(25, 25, 25); color: #eee;">
            <header style="padding: 10px; background: rgb(25, 25, 25); display: flex; align-items: center; gap: 20px; border-bottom: 1px solid rgb(45, 45, 45);">
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
                    on:change={
                        let manager_framer = manager.clone();
                        move |ev| {
                            let val = event_target_value(&ev);
                            manager_framer.set_framer(val);
                        }
                    }
                >
                    <option value="lines">Lines</option>
                    <option value="raw" selected>Raw</option>
                    <option value="cobs">COBS</option>
                    <option value="slip">SLIP</option>
                </select>

                // Encoder / Auto-Decoder Dropdown Removed (Implicit now)


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
            <div style="flex: 1; display: flex; overflow: hidden; height: 100%; flex-direction: row;">
                 // Sidebar
                <div style="flex: 1; position: relative; overflow: hidden; display: flex;">
                    // Terminal Container
                    <div style=move || format!("flex: 1; height: 100%; display: {};", if view_mode.get() == ViewMode::Terminal { "block" } else { "none" })>
                         <xterm::TerminalView 
                             on_mount=on_terminal_mount
                             on_terminal_ready=on_term_ready
                         />
                    </div>
                    
                    // Hex View Container
                    <Show when=move || view_mode.get() == ViewMode::Hex fallback=|| ()>
                        <hex_view::HexView events=events_list />
                    </Show>
                </div>

                 // Sidebar (Moved to Right)
                 <div style="width: 50px; background: rgb(25, 25, 25); display: flex; flex-direction: column; align-items: center; padding-top: 10px; border-left: 1px solid rgb(45, 45, 45);">
                    // Terminal Button
                    <button
                        title="Terminal View (UTF-8)"
                        style=move || format!(
                            "width: 40px; height: 40px; background: {}; color: white; border: none; cursor: pointer; border-radius: 4px; margin-bottom: 8px; display: flex; align-items: center; justify-content: center;",
                            if view_mode.get() == ViewMode::Terminal { "rgb(45, 45, 45)" } else { "transparent" }
                        )
                        on:click={
                            let m = manager.clone();
                            move |_| {
                                set_view_mode.set(ViewMode::Terminal);
                                set_events_list.set(Vec::new()); // Clear history on switch
                                m.set_decoder("utf8".to_string());
                            }
                        }
                    >
                        {xterm::icon()}
                    </button>
                    
                    // Hex Inspector Button
                    <button
                        title="Hex Inspector (Hex List)"
                        style=move || format!(
                            "width: 40px; height: 40px; background: {}; color: white; border: none; cursor: pointer; border-radius: 4px; margin-bottom: 8px; display: flex; align-items: center; justify-content: center;",
                            if view_mode.get() == ViewMode::Hex { "rgb(45, 45, 45)" } else { "transparent" }
                        )
                        on:click={
                            let m = manager.clone();
                            move |_| {
                                set_view_mode.set(ViewMode::Hex);
                                set_events_list.set(Vec::new()); // Clear history on switch
                                m.set_decoder("hex".to_string());
                            }
                        }
                    >
                         {hex_view::icon()}
                    </button>
                 </div>
            </div>
        </div>
    }
}



