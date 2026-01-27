use crate::protocol::WorkerToUi;
use core_types::{DecodedEvent, RawEvent, SelectionRange};
use leptos::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{MessageEvent, Worker};
// Imports Cleaned

// Actor system (replaces ConnectionManager)
mod actor_bridge;
mod actor_system;
use actor_bridge::ActorBridge;
use actor_protocol::ConnectionState;

mod hex_view;
// mod mavlink_view; // Removed duplicate
pub mod protocol;
mod terminal_metadata;
pub mod worker_logic;
mod xterm;

pub mod mavlink_view;
mod ui;
use ui::{Sidebar, ViewMode};

// Data retention limits for the unified raw log
/// Maximum raw log size in bytes (10 MB)
const MAX_LOG_BYTES: usize = 10 * 1024 * 1024;

/// Maximum number of raw log events (safety fallback)
const MAX_LOG_EVENTS: usize = 10000;

/// Maximum number of decoded events to retain
const MAX_DECODED_EVENTS: usize = 2500;

#[component]
pub fn App() -> impl IntoView {
    let (_terminal_ready, set_terminal_ready) = create_signal(false);
    let (is_webserial_supported, set_is_webserial_supported) = create_signal(true);

    // Worker Signal (Used by ActorBridge)
    let (worker, set_worker) = create_signal::<Option<Worker>>(None);
    let (view_mode, set_view_mode) = create_signal(ViewMode::Terminal);

    // Actor System (replaces ConnectionManager)
    let manager_internal = actor_system::create_actor_system();
    let manager = ActorBridge::new(manager_internal, worker.into());
    let status = manager.get_status();

    // Derive connected signal from state machine
    let state_signal = manager.state;
    let connected = Signal::derive(move || state_signal.get() == ConnectionState::Connected);

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

    // ========== Data Architecture: Unified Raw Log + Per-Decoder Views ==========
    //
    // Architecture:
    // 1. raw_log: Unified append-only log of all RawEvents (bytes + timestamp + channel)
    //    - Populated from worker DataBatch frames
    //    - Byte-based capping (10MB / 10k events)
    //    - Survives decoder view switches
    //    - Source of truth for Hex view
    //
    // 2. events_list: Worker-generated DecodedEvents (protocol-specific parsing)
    //    - Populated from worker DataBatch events
    //    - Used by MAVLink view (filters by protocol)
    //    - No longer cleared on view switch (history persists)
    //    - Future: Could be replaced by per-view decoding of raw_log
    //
    // 3. Per-decoder cursors: Track processing position for each view
    //    - hex_cursor: HexView scroll/processing position
    //    - MAVLink uses timestamp-based cursor internally
    //
    // Benefits:
    // ✅ History persists when switching between decoder views
    // ✅ Each view maintains independent state (scroll, processed events)
    // ✅ Foundation for future features (replay, bookmarks, multi-view)

    let (events_list, set_events_list) = create_signal::<Vec<DecodedEvent>>(Vec::new());
    let (raw_log, set_raw_log) = create_signal::<Vec<RawEvent>>(Vec::new());
    // Cumulative byte counter for raw_log to avoid O(N) iteration
    let (raw_log_bytes, set_raw_log_bytes) = create_signal(0usize);
    let (hex_cursor, set_hex_cursor) = create_signal(0usize);

    // ========== Cross-View Selection Sync ==========
    // Global selection state for synchronizing selections across Terminal, Hex, and future views
    let (global_selection, set_global_selection) = create_signal::<Option<SelectionRange>>(None);

    // Terminal metadata for mapping between Terminal text and raw_log byte positions
    let (terminal_metadata, set_terminal_metadata) =
        create_signal(terminal_metadata::TerminalMetadata::new());

    // Legacy signals removed/replaced by manager:
    // status, connected, transport, active_port, is_reconfiguring

    create_effect(move |_| {
        if let Some(window) = web_sys::window() {
            let nav = window.navigator();
            let serial = nav.serial();
            if serial.is_undefined() {
                set_is_webserial_supported.set(false);
            }
        }
    });

    // Worker Logic
    let manager_worker_init = manager.clone();
    create_effect(move |_| {
        let manager = manager_worker_init.clone();
        if let Ok(w) = Worker::new("worker_bootstrap.js") {
            // Restore TextDecoder for RX to Main Thread (if we ever want to decode locally? No,
            // worker does that) But wait, worker sends BACK a 'DataBatch' with frames.
            // We need to print raw text to terminal.
            // The worker parses frames. Does it decode text?
            // Looking at worker_logic.rs:
            // It receives IngestData -> Frames -> Decoder.
            // It sends back DataBatch { frames, events }.
            // Frames contain raw bytes.
            // So Main Thread needs to decode bytes to string for Xterm.

            let Ok(decoder) = web_sys::TextDecoder::new() else {
                manager
                    .set_status
                    .set("Failed to create TextDecoder".into());
                return;
            };
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
                            // Update unified raw log with frames
                            if !frames.is_empty() {
                                set_raw_log.update(|log| {
                                    // Append new raw events and update byte counter
                                    let mut bytes_added = 0;
                                    for frame in &frames {
                                        let event = RawEvent::from_frame(frame);
                                        bytes_added += event.byte_size();
                                        log.push(event);
                                    }

                                    // Update cumulative byte counter
                                    let total_bytes = raw_log_bytes.get_untracked() + bytes_added;
                                    set_raw_log_bytes.set(total_bytes);

                                    if total_bytes > MAX_LOG_BYTES || log.len() > MAX_LOG_EVENTS {
                                        // Trim oldest events until under limit
                                        let mut trimmed = 0;
                                        let mut bytes_removed = 0;

                                        while (total_bytes - bytes_removed > MAX_LOG_BYTES
                                            || log.len() - trimmed > MAX_LOG_EVENTS)
                                            && trimmed < log.len()
                                        {
                                            if let Some(event) = log.get(trimmed) {
                                                bytes_removed += event.byte_size();
                                            }
                                            trimmed += 1;
                                        }

                                        if trimmed > 0 {
                                            log.drain(0..trimmed);

                                            // Update cumulative byte counter after trimming
                                            set_raw_log_bytes.set(total_bytes - bytes_removed);

                                            // Adjust terminal_metadata for the trimmed bytes
                                            set_terminal_metadata.update(|meta| {
                                                meta.adjust_for_log_trim(bytes_removed);
                                            });
                                        }
                                    }
                                });
                            }

                            // Terminal direct write - always write to maintain metadata mapping
                            // Terminal exists even when view is hidden, and we need complete
                            // metadata for cross-view selection sync to
                            // work
                            if let Some(term) = term_handle.get_untracked() {
                                for f in &frames {
                                    if !f.bytes.is_empty() {
                                        if let Ok(text) = decoder
                                            .decode_with_u8_array_and_options(&f.bytes, &opts)
                                        {
                                            let text: String = text;
                                            if !text.is_empty() {
                                                term.write(&text);

                                                // Record metadata for cross-view selection sync
                                                // This must happen for ALL data, not just when
                                                // Terminal is visible
                                                set_terminal_metadata.update(|meta| {
                                                    meta.record_write(
                                                        &f.bytes,
                                                        &text,
                                                        f.timestamp_us,
                                                    );
                                                });
                                            }
                                        }
                                    }
                                }
                            }

                            // Update events
                            if !events.is_empty() {
                                set_events_list.update(|list| {
                                    list.extend(events);
                                    // Cap at MAX_DECODED_EVENTS to ensure we don't drop high-freq
                                    // MAVLink packets
                                    // before the View effect can process them.
                                    // 500 was too aggressive for 50Hz streams.
                                    if list.len() > MAX_DECODED_EVENTS {
                                        let split = list.len() - MAX_DECODED_EVENTS;
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
                        WorkerToUi::TxData { data } => {
                            let m = manager.clone();
                            spawn_local(async move {
                                let _ = m.write(&data).await;
                            });
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
        // Use state machine to determine button behavior
        let current_state = manager_disc.state.get();

        #[cfg(debug_assertions)]
        web_sys::console::log_1(
            &format!(
                "DEBUG: Button clicked - state={:?}, force_picker={}, can_disconnect={}",
                current_state,
                force_picker,
                current_state.can_disconnect()
            )
            .into(),
        );

        // Allow disconnect if state allows it (Connected, AutoReconnecting, or DeviceLost)
        if current_state.can_disconnect() && !force_picker {
            // Disconnect Logic - cancels auto-reconnect OR disconnects active connection
            #[cfg(debug_assertions)]
            web_sys::console::log_1(&"DEBUG: Executing disconnect logic".into());
            let manager_d = manager_disc.clone();
            spawn_local(async move {
                manager_d.disconnect().await;
            });
            return;
        }

        #[cfg(debug_assertions)]
        web_sys::console::log_1(
            &format!(
                "DEBUG: Executing connect logic (can_disconnect={}, force_picker={})",
                current_state.can_disconnect(),
                force_picker
            )
            .into(),
        );

        // Reset detected info
        manager.set_detected_baud.set(0);
        manager.set_detected_framing.set("".into());

        let current_baud = baud_rate.get_untracked();

        // Load device from localStorage (same key as ReconnectActor)
        let storage = web_sys::window().and_then(|w| w.local_storage().ok().flatten());
        let device = storage
            .as_ref()
            .and_then(|s| s.get_item("futureterm_last_device").ok().flatten())
            .and_then(|value| {
                // Parse "0403:6001" format (hex)
                let parts: Vec<&str> = value.split(':').collect();
                if parts.len() == 2 {
                    let vid = parts.first().and_then(|s| u16::from_str_radix(s, 16).ok());
                    let pid = parts.get(1).and_then(|s| u16::from_str_radix(s, 16).ok());
                    if let (Some(v), Some(p)) = (vid, pid) {
                        return Some((v, p));
                    }
                }
                None
            });

        let (init_vid, init_pid) = match device {
            Some((vid, pid)) => (Some(vid), Some(pid)),
            None => (None, None),
        };

        #[cfg(debug_assertions)]
        web_sys::console::log_1(
            &format!(
                "DEBUG: Loaded from localStorage: VID={:04X?}, PID={:04X?}",
                init_vid, init_pid
            )
            .into(),
        );

        let (last_vid, set_last_vid) = create_signal::<Option<u16>>(init_vid);
        let (last_pid, set_last_pid) = create_signal::<Option<u16>>(init_pid);
        let manager = manager_con_main.clone();

        spawn_local(async move {
            let Some(window) = web_sys::window() else {
                manager
                    .set_status
                    .set("Error: window not available.".into());
                return;
            };
            let nav = window.navigator();
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
                if manager.state.get_untracked() == ConnectionState::Connected {
                    manager.set_status.set("Switching Port...".into());
                    manager.disconnect().await;
                }

                // Capture VID/PID for Reconnect
                // Note: SerialPortInfo has usb_vendor_id() and usb_product_id() methods,
                // but they may not be present for all port types (e.g., virtual COM ports).
                // We use Reflect to safely handle missing properties.
                let info = port.get_info();
                let vid = js_sys::Reflect::get(&info, &"usbVendorId".into())
                    .ok()
                    .and_then(|v| v.as_f64())
                    .map(|v| v as u16);
                let pid = js_sys::Reflect::get(&info, &"usbProductId".into())
                    .ok()
                    .and_then(|v| v.as_f64())
                    .map(|v| v as u16);

                // CRITICAL FIX: VID/PID will be cached ONLY after successful connection
                // (moved to Ok(_) branch below to prevent caching wrong device)

                let current_framing = framing.get_untracked();

                // Determine if we should auto-detect baud rate
                let stored_vid = last_vid.get_untracked();
                let stored_pid = last_pid.get_untracked();
                let device_changed = stored_vid != vid || stored_pid != pid;
                let fresh_session = stored_vid.is_none() || stored_pid.is_none();

                // Auto-detect baud when:
                // 1. Fresh session (no stored device)
                // 2. Device swapped (different VID/PID)
                // 3. User explicitly set baud=0 in UI
                let final_baud = if fresh_session || device_changed {
                    #[cfg(debug_assertions)]
                    if fresh_session {
                        web_sys::console::log_1(
                            &"DEBUG: Fresh session detected, will auto-detect baud rate".into(),
                        );
                    } else {
                        web_sys::console::log_1(
                            &format!(
                                "DEBUG: Device changed (stored {:04X?}:{:04X?}, selected {:04X?}:{:04X?}), will auto-detect baud",
                                stored_vid, stored_pid, vid, pid
                            )
                            .into(),
                        );
                    }
                    0 // Auto-detect
                } else {
                    current_baud // Use stored/UI baud
                };

                // Cache VID/PID for auto-reconnect (ALWAYS, regardless of auto-detect)
                set_last_vid.set(vid);
                set_last_pid.set(pid);

                // Save to LocalStorage (same key as ReconnectActor)
                // CRITICAL FIX: Save VID/PID for ALL connections, not just auto-detect
                if let (Some(v), Some(p)) = (vid, pid) {
                    if let Some(window) = web_sys::window() {
                        if let Ok(Some(storage)) = window.local_storage() {
                            let value = format!("{:04X}:{:04X}", v, p);
                            let _ = storage.set_item("futureterm_last_device", &value);
                        }
                    }
                }

                // Resolve Auto to something concrete if needed, but Manager handles it now.
                // if current_baud == 0 { final_baud = 115200; } // REMOVED (Regression Fix)
                if final_baud == 0 || current_framing == "Auto" {
                    manager.set_status.set("Auto-Detecting Config...".into());

                    web_sys::console::log_1(
                        &format!("Smart Port Check: VID={:?} PID={:?}", vid, pid).into(),
                    );

                    let manager_conn = manager.clone();
                    spawn_local(async move {
                        // ActorBridge handles detection if baud == 0
                        manager_conn
                            .connect(port, final_baud, &current_framing)
                            .await;
                    });

                    // --- Auto-Reconnect Listeners ---
                    // Delegated to ConnectionManager
                    manager.setup_auto_reconnect(
                        last_vid.into(),
                        last_pid.into(),
                        set_last_vid,
                        set_last_pid,
                        baud_rate.into(),
                        detected_baud,
                        framing.into(),
                    );
                } else {
                    // Manual baud rate connection
                    let manager_conn = manager.clone();
                    spawn_local(async move {
                        manager_conn
                            .connect(port, final_baud, &current_framing)
                            .await;
                    });

                    // --- Auto-Reconnect Listeners ---
                    manager.setup_auto_reconnect(
                        last_vid.into(),
                        last_pid.into(),
                        set_last_vid,
                        set_last_pid,
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
                // (unless we add a "Re-Scan" button later, but for now this prevents redundant
                // loops if both set to Auto) Allow Auto (0 / Auto) to trigger
                // reconfiguration too

                #[cfg(debug_assertions)]
                web_sys::console::log_1(&"Dynamically Reconfiguring Port...".into());

                // Manager Reconfigure (Handles Close -> Open -> Loop)
                // Pass `b` and `f` directly. If b=0, Manager detects. If f=Auto, Manager probes.
                manager_r.reconfigure(b, f);
            });
        }
    });

    // Auto-Switch View to MAVLink Dashboard
    create_effect(move |_| {
        let dec = manager.decoder_id.get();
        if dec == "mavlink" && view_mode.get_untracked() != ViewMode::Mavlink {
            set_view_mode.set(ViewMode::Mavlink);
            // History now persists across decoder switches
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
                    <option value="19200">19200</option>
                    <option value="38400">38400</option>
                    <option value="57600">57600</option>
                    <option value="115200">115200</option>
                    <option value="230400">230400</option>
                    <option value="460800">460800</option>
                    <option value="500000">500000</option>
                    <option value="921600">921600</option>
                    <option value="1000000">1000000</option>
                    <option value="1500000">1500000</option>
                    <option value="2000000">2000000</option>
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
                            use core_types::FramerId;
                            use std::str::FromStr;
                            let val = event_target_value(&ev);
                            if let Ok(framer) = FramerId::from_str(&val) {
                                manager_framer.set_framer_typed(framer);
                            }
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
                    // Use state machine to determine indicator color and animation
                    let current_state = manager.state.get();
                    let color = current_state.indicator_color();
                    let animation = if current_state.indicator_should_pulse() {
                        "animation: pulse 0.3s ease-in-out infinite;"
                    } else {
                        ""
                    };

                    format!("width: 12px; height: 12px; border-radius: 50%; background: {}; transition: background 0.3s ease; {}", color, animation)
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
                    "@keyframes pulse {
                        0%, 100% { opacity: 1; }
                        50% { opacity: 0.4; }
                    }
                    .split-btn { transition: background-color 0.2s; }
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
                        {move || {
                            // Use state machine to determine button text
                            if manager.state.get().button_shows_disconnect() {
                                "Disconnect"
                            } else {
                                "Connect"
                            }
                        }}
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
                             terminal_metadata=terminal_metadata
                             global_selection=global_selection
                             set_global_selection=set_global_selection
                         />
                    </div>

                    // Hex View Container
                    <Show when=move || view_mode.get() == ViewMode::Hex fallback=|| ()>
                        <hex_view::HexView
                            raw_log=raw_log
                            cursor=hex_cursor
                            set_cursor=set_hex_cursor
                            global_selection=global_selection
                            set_global_selection=set_global_selection
                        />
                    </Show>

                    // MAVLink View Container
                    <Show when=move || view_mode.get() == ViewMode::Mavlink fallback=|| ()>
                        <mavlink_view::MavlinkView events_list=events_list connected=connected />
                    </Show>
                </div>

                 // Sidebar (Moved to Right)
                 <Sidebar view_mode=view_mode.into() set_view_mode=set_view_mode manager=manager.clone() />
            </div>
        </div>
    }
}
