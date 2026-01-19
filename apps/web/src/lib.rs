use crate::protocol::WorkerToUi;
use core_types::DecodedEvent;
use leptos::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{MessageEvent, Worker};
// Imports Cleaned

mod connection;
use connection::ConnectionManager;

mod hex_view;
pub mod protocol;
pub mod worker_logic;
mod xterm;

#[derive(Clone, Copy, PartialEq)]
enum ViewMode {
    Terminal,
    Hex,
}

#[component]
pub fn App() -> impl IntoView {
    let (_terminal_ready, set_terminal_ready) = create_signal(false);
    let (is_webserial_supported, set_is_webserial_supported) = create_signal(true);

    // Worker Signal (Used by ConnectionManager)
    let (worker, set_worker) = create_signal::<Option<Worker>>(None);
    let (view_mode, set_view_mode) = create_signal(ViewMode::Terminal);

    // Connection Manager encapsulates Status, Connected, Transport, Port
    let manager = ConnectionManager::new(worker.into());
    let status = manager.get_status();
    let connected = manager.get_connected();

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
        let nav = web_sys::window().unwrap().navigator();
        let serial = nav.serial();
        if serial.is_undefined() {
            set_is_webserial_supported.set(false);
        }
    });

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
                        }
                        WorkerToUi::DataBatch { frames, events } => {
                            if let Some(term) = term_handle.get_untracked() {
                                for f in frames {
                                    if !f.bytes.is_empty() {
                                        if let Ok(text) = decoder
                                            .decode_with_u8_array_and_options(&f.bytes, &opts)
                                        {
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
                        }
                        WorkerToUi::AnalyzeResult { baud_rate, score } => {
                            // Received analysis from worker (if we used worker mode)
                            web_sys::console::log_1(
                                &format!("Worker Analysis: Baud {} Score {:.2}", baud_rate, score)
                                    .into(),
                            );
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
        let init_vid = storage
            .as_ref()
            .and_then(|s| s.get_item("last_vid").ok().flatten())
            .and_then(|s| s.parse::<u16>().ok());
        let init_pid = storage
            .as_ref()
            .and_then(|s| s.get_item("last_pid").ok().flatten())
            .and_then(|s| s.parse::<u16>().ok());

        let (last_vid, set_last_vid) = create_signal::<Option<u16>>(init_vid);
        let (last_pid, set_last_pid) = create_signal::<Option<u16>>(init_pid);
        let manager = manager_con_main.clone();

        spawn_local(async move {
            let nav = web_sys::window().unwrap().navigator();
            let serial = nav.serial();

            if serial.is_undefined() {
                manager
                    .set_status
                    .set("Error: WebSerial not supported.".into());
                return;
            }

            let mut final_port: Option<web_sys::SerialPort> = None;

            // 1. Smart Check
            if !shift_held {
                final_port = manager
                    .auto_select_port(last_vid.get_untracked(), last_pid.get_untracked())
                    .await;
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
                let vid = js_sys::Reflect::get(&info, &"usbVendorId".into())
                    .ok()
                    .and_then(|v| v.as_f64())
                    .map(|v| v as u16);
                let pid = js_sys::Reflect::get(&info, &"usbProductId".into())
                    .ok()
                    .and_then(|v| v.as_f64())
                    .map(|v| v as u16);

                // Store for reconnect
                set_last_vid.set(vid);
                set_last_pid.set(pid);

                set_last_vid.set(vid);
                set_last_pid.set(pid);

                let current_framing = framing.get_untracked();

                let final_baud = current_baud;
                // Removed unused final_framing_str variable

                // Resolve Auto to something concrete if needed, but Manager handles it now.
                // if current_baud == 0 { final_baud = 115200; } // REMOVED (Regression Fix)
                if final_baud == 0 || current_framing == "Auto" {
                    manager.set_status.set("Auto-Detecting Config...".into());

                    web_sys::console::log_1(
                        &format!("Smart Port Check: VID={:?} PID={:?}", vid, pid).into(),
                    );

                    let manager_conn = manager.clone();
                    spawn_local(async move {
                        // Manager handles detection if baud == 0
                        match manager_conn
                            .connect(port, current_baud, &current_framing)
                            .await
                        {
                            Ok(_) => {
                                // Save to LocalStorage
                                if let (Some(v), Some(p)) = (vid, pid) {
                                    if let Ok(Some(storage)) =
                                        web_sys::window().unwrap().local_storage()
                                    {
                                        let _ = storage.set_item("last_vid", &v.to_string());
                                        let _ = storage.set_item("last_pid", &p.to_string());
                                    }
                                }
                            }
                            Err(_) => {
                                // Status updated by manager
                            }
                        }
                    });

                    // --- Auto-Reconnect Listeners ---
                    // Delegated to ConnectionManager
                    manager.setup_auto_reconnect(
                        last_vid.into(),
                        last_pid.into(),
                        baud_rate.into(),
                        detected_baud,
                        framing.into(),
                    );
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
            <Show when=move || !is_webserial_supported.get() fallback=|| ()>
                <div style="position: fixed; top: 0; left: 0; width: 100vw; height: 100vh; background: rgba(15, 15, 15, 0.98); z-index: 9999; display: flex; flex-direction: column; align-items: center; justify-content: center; text-align: center; color: white;">
                    <h1 style="font-family: 'Magneto', 'Impact', sans-serif; font-size: 3rem; margin-bottom: 2rem; color: #ff5555; text-shadow: 0 0 10px rgba(255, 85, 85, 0.3);">Browser Not Supported</h1>
                    <p style="font-size: 1.2rem; max-width: 800px; line-height: 1.6; color: #ccc; margin-bottom: 3rem;">
                        FutureTerm requires the <strong>WebSerial API</strong> to communicate with hardware devices.<br/>
                        This feature is currently missing from your browser (e.g., Safari, Firefox).
                    </p>

                    <div style="display: flex; gap: 30px; flex-wrap: wrap; justify-content: center;">
                         <div style="padding: 20px 40px; background: #252525; border-radius: 12px; border: 1px solid #444; text-align: center;">
                            <div style="font-weight: bold; font-size: 1.1rem; margin-bottom: 10px; color: #4CAF50;">Supported Browsers</div>
                            <div style="font-size: 1.5rem;">Chrome, Edge, Opera</div>
                        </div>
                    </div>
                </div>
            </Show>

            <header style="padding: 10px; background: rgb(25, 25, 25); display: flex; align-items: center; gap: 10px; border-bottom: 1px solid rgb(45, 45, 45);">
                <h1 style="margin: 0; font-family: 'Magneto', 'Impact', 'Arial Black', sans-serif; font-style: italic; font-size: 1.5rem; font-weight: normal; letter-spacing: 1px;">FutureTerm</h1>
                <div style="flex: 1;"></div>

                <span style="font-size: 0.9rem; color: #aaa;">{move || status.get()}</span>

                <select
                    style="width: 140px; background: #333; color: white; border: 1px solid #555; padding: 4px; border-radius: 4px;"
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
                    style="width: 110px; background: #333; color: white; border: 1px solid #555; padding: 4px; border-radius: 4px;"
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
                    style="width: 80px; background: #333; color: white; border: 1px solid #555; padding: 4px; border-radius: 4px;"
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


                // Status Light
                <div style=move || {
                    let is_conn = connected.get();
                     // Use status text as proxy for "Connecting..." since is_reconfiguring might not cover initial connection
                    let s = status.get();
                    let is_busy = s.to_lowercase().contains("connecting") || s.to_lowercase().contains("scanning") || s.to_lowercase().contains("reconfiguring");

                    let color = if is_conn && !is_busy {
                        "rgb(95, 200, 85)" // Connected (Green)
                    } else if is_busy {
                        "rgb(245, 190, 80)" // Connecting/Busy (Orange)
                    } else {
                        "rgb(240, 105, 95)" // Disconnected (Red)
                    };

                    format!("width: 12px; height: 12px; border-radius: 50%; background: {}; transition: background 0.3s ease;", color)
                }></div>

                // RX/TX Indicators (Compact Stack)
                <div style="display: flex; flex-direction: column; align-items: flex-end; justify-content: center; gap: 2px;">
                    // TX
                    <div style="display: flex; align-items: center; gap: 6px; line-height: 1;">
                         <span style="font-family: sans-serif; font-size: 0.6rem; font-weight: bold; color: #ccc;">TX</span>
                         <div style=move || {
                             let active = manager.tx_active.get();
                             let (color, shadow) = if active {
                                 ("rgb(80, 255, 80)", "0 0 4px rgb(80, 255, 80)")
                             } else {
                                 ("rgb(60, 60, 60)", "none")
                             };
                             format!("width: 5px; height: 5px; border-radius: 50%; background: {}; box-shadow: {}; transition: background 0.05s;", color, shadow)
                         }></div>
                    </div>
                    // RX
                    <div style="display: flex; align-items: center; gap: 6px; line-height: 1;">
                         <span style="font-family: sans-serif; font-size: 0.6rem; font-weight: bold; color: #ccc;">RX</span>
                         <div style=move || {
                             let active = manager.rx_active.get();
                             let (color, shadow) = if active {
                                 ("rgb(255, 50, 50)", "0 0 4px rgb(255, 50, 50)")
                             } else {
                                 ("rgb(60, 60, 60)", "none")
                             };
                             format!("width: 5px; height: 5px; border-radius: 50%; background: {}; box-shadow: {}; transition: background 0.05s;", color, shadow)
                         }></div>
                    </div>
                </div>

                <style>
                    {
                    ".split-btn { transition: background-color 0.2s; }
                    .split-btn:hover { background-color: #0062a3 !important; }
                    .split-btn:active { background-color: #005a96 !important; }"
                    }
                </style>
                <div style="display: flex; align-items: stretch; height: 28px; border-radius: 4px; overflow: hidden;">
                    <button
                        class="split-btn"
                        style="padding: 0 12px; width: 100px; text-align: center; background: #007acc; color: white; border: none; cursor: pointer; font-size: 0.9rem; border-right: 1px solid rgba(255,255,255,0.2);"
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
