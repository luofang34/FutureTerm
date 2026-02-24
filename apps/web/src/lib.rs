use core_types::Transport;
use leptos::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::Worker;

// Actor system (replaces ConnectionManager)
mod actor_bridge;
mod actor_system;
use actor_bridge::ActorBridge;

mod connect;
mod context;
use context::create_app_context;

mod data_dispatch;

mod hex_view;
pub mod protocol;
mod terminal_metadata;
pub mod worker_logic;
mod xterm;

pub mod mavlink_view;
mod ui;
use ui::{Sidebar, ViewMode};

#[component]
pub fn App() -> impl IntoView {
    // Actor System (replaces ConnectionManager)
    let manager_internal = actor_system::create_actor_system();
    // Worker signal must be created before ActorBridge (which reads it)
    let (worker, set_worker) = create_signal::<Option<Worker>>(None);
    let manager = ActorBridge::new(manager_internal, worker.into());

    // Create centralised application context (all shared signals)
    let ctx = create_app_context(manager.clone(), worker, set_worker);
    provide_context(ctx.clone());

    let status = manager.get_status();
    let detected_baud = manager.detected_baud;
    let detected_framing = manager.detected_framing;

    // Local aliases for closures that capture individual Copy/Clone fields.
    // Only extract what is actually used in lib.rs — the connect logic
    // is accessed via connect::on_connect, and worker dispatch signals
    // are accessed directly via ctx inside data_dispatch.rs.
    let set_terminal_ready = ctx.set_terminal_ready;
    let show_bridge_install = ctx.show_bridge_install;
    let set_show_bridge_install = ctx.set_show_bridge_install;
    let view_mode = ctx.view_mode;
    let set_view_mode = ctx.set_view_mode;
    let connected = ctx.connected;
    let baud_rate = ctx.baud_rate;
    let set_baud_rate = ctx.set_baud_rate;
    let framing = ctx.framing;
    let set_framing = ctx.set_framing;
    let active_framing = ctx.active_framing;
    let set_term_handle = ctx.set_term_handle;
    let raw_log = ctx.raw_log;
    let hex_cursor = ctx.hex_cursor;
    let set_hex_cursor = ctx.set_hex_cursor;
    let global_selection = ctx.global_selection;
    let set_global_selection = ctx.set_global_selection;
    let terminal_metadata = ctx.terminal_metadata;
    let events_list = ctx.events_list;
    let bridge_active = ctx.bridge_active.clone();
    let bridge_pending_baud = ctx.bridge_pending_baud.clone();
    let bridge_ports = ctx.bridge_ports;
    let set_bridge_port_pick = ctx.set_bridge_port_pick;
    let bridge_tx_queue = ctx.bridge_tx_queue.clone();

    // ── Startup pre-checks ──
    // Detect transport availability at page load so the Connect button can
    // act instantly instead of spending 3+ seconds probing.
    //
    // WebSerial check is synchronous (<1ms).  Bridge daemon probe is async
    // but a TCP connection-refused on localhost resolves in <10ms, so both
    // results are typically ready well before the user clicks anything.
    let has_webserial = web_sys::window()
        .map(|w| !w.navigator().serial().is_undefined())
        .unwrap_or(false);

    if !has_webserial {
        let set_install = set_show_bridge_install;
        let set_bridge_ready = ctx.set_bridge_ready;
        spawn_local(async move {
            let ws_url = "wss://local.futureterm.app:9876";

            // 1. Quick probe -- daemon may already be running.
            let mut ws = transport_websocket::WebSocketTransport::new();
            if ws.connect(ws_url).await.is_ok() {
                let _ = ws.close().await;
                set_bridge_ready.set(Some(true));
                return;
            }

            // 2. Daemon not running -- try URL scheme launch.
            if let Some(window) = web_sys::window() {
                if let Some(doc) = window.document() {
                    if let Ok(iframe) = doc.create_element("iframe") {
                        let _ = iframe.set_attribute("style", "display:none");
                        let _ = iframe.set_attribute("src", "futureterm://launch?port=9876");
                        if let Some(body) = doc.body() {
                            let _ = body.append_child(&iframe);
                            let body_clone = body.clone();
                            let iframe_clone = iframe.clone();
                            let cleanup = wasm_bindgen::closure::Closure::once(move || {
                                let _ = body_clone.remove_child(&iframe_clone);
                            });
                            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                                cleanup.as_ref().unchecked_ref(),
                                1000,
                            );
                            cleanup.forget();
                        }
                    }
                }
            }

            // 3. Fast retries while daemon starts up.
            //    Cumulative: 500 / 1000 / 2000 / 3500ms.
            for &delay in &[500u32, 500, 1000, 1500] {
                gloo_timers::future::TimeoutFuture::new(delay).await;
                let mut probe = transport_websocket::WebSocketTransport::new();
                if probe.connect(ws_url).await.is_ok() {
                    let _ = probe.close().await;
                    set_bridge_ready.set(Some(true));
                    return;
                }
            }

            // 4. Helper not installed -- show dialog and keep polling.
            set_bridge_ready.set(Some(false));
            set_install.set(true);

            for _ in 0..300 {
                gloo_timers::future::TimeoutFuture::new(1000).await;
                if !show_bridge_install.get() {
                    return; // User clicked Cancel
                }
                let mut probe = transport_websocket::WebSocketTransport::new();
                if probe.connect(ws_url).await.is_ok() {
                    let _ = probe.close().await;
                    set_bridge_ready.set(Some(true));
                    set_install.set(false);
                    return;
                }
            }
        });
    }

    // Worker Logic (extracted to data_dispatch module)
    data_dispatch::setup_worker_dispatch(&ctx);

    // Bridge mode clones for closures that still live in lib.rs
    let bridge_active_reconf = bridge_active.clone();
    let bridge_pending_baud_reconf = bridge_pending_baud.clone();
    let bridge_active_term = bridge_active.clone();
    let bridge_tx_queue_term = bridge_tx_queue.clone();

    // Connect logic (extracted to connect module)
    let on_connect = {
        let ctx = ctx.clone();
        move |force_picker: bool| connect::on_connect(&ctx, force_picker)
    };

    // --- Dynamic Reconfiguration Effect ---
    let manager_reconf = manager.clone();

    create_effect(move |_| {
        let b = baud_rate.get();
        let f = framing.get();
        let af = active_framing.get();

        if connected.get_untracked() {
            if bridge_active_reconf.get() {
                // Bridge mode: signal pending baud change; bridge loop applies it.
                // Only baud changes are supported; framing is handled by the worker.
                if b > 0 {
                    bridge_pending_baud_reconf.set(b);
                }
            } else {
                // WebSerial mode: use existing reconfigure path
                let manager_r = manager_reconf.clone();
                spawn_local(async move {
                    #[cfg(debug_assertions)]
                    web_sys::console::log_1(&"Dynamically Reconfiguring Port...".into());
                    manager_r.reconfigure(b, f, af);
                });
            }
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
        let bridge_active_tx = bridge_active_term.clone();
        let bridge_tx_queue_tx = bridge_tx_queue_term.clone();
        let on_data_cb = Closure::wrap(Box::new(move |data: JsValue| {
            if let Some(text) = data.as_string() {
                let bytes = text.into_bytes();

                if bridge_active_tx.get() {
                    // Bridge mode - queue for WS send
                    #[cfg(debug_assertions)]
                    web_sys::console::log_1(
                        &format!("Bridge TX: queuing {} bytes", bytes.len()).into(),
                    );
                    bridge_tx_queue_tx.borrow_mut().push(bytes);
                } else {
                    // WebSerial mode
                    let active_manager = manager_tx.clone();
                    spawn_local(async move {
                        if let Err(e) = active_manager.write(&bytes).await {
                            #[cfg(debug_assertions)]
                            web_sys::console::log_1(&format!("TX Error: {:?}", e).into());
                        }
                    });
                }
            }
        }) as Box<dyn FnMut(JsValue)>);

        t.on_data(on_data_cb.into_js_value().unchecked_into());
    });

    view! {
        <div style="display: flex; flex-direction: column; height: 100vh; background: rgb(25, 25, 25); color: #eee;">
            // Safari/Firefox bridge helper install dialog
            <Show when=move || show_bridge_install.get() fallback=|| ()>
                <div style="position: fixed; top: 0; left: 0; width: 100vw; height: 100vh; background: rgba(0,0,0,0.7); z-index: 10000; display: flex; align-items: center; justify-content: center;">
                    <div style="background: #2a2a2a; border: 1px solid #555; border-radius: 8px; padding: 24px 32px; max-width: 480px; color: #eee; font-family: sans-serif;">
                        <h2 style="margin: 0 0 12px; font-size: 1.2rem; color: #ff9800;">"Serial Port Helper Required"</h2>
                        <p style="margin: 0 0 8px; font-size: 0.9rem; line-height: 1.5; color: #ccc;">
                            "Your browser doesn\u{2019}t support the WebSerial API. FutureTerm needs a small helper app running locally to access your serial ports."
                        </p>
                        <p style="margin: 0 0 16px; font-size: 0.9rem; line-height: 1.5; color: #ccc;">
                            "The helper is lightweight (~1 MB), runs only when needed, and shuts down automatically after 2 minutes of inactivity."
                        </p>
                        <div style="display: flex; gap: 12px; justify-content: flex-end; align-items: center;">
                            <button
                                style="padding: 8px 16px; background: #444; color: #ccc; border: 1px solid #666; border-radius: 4px; cursor: pointer; font-size: 0.9rem; line-height: 1.4;"
                                on:click=move |_| set_show_bridge_install.set(false)>
                                "Cancel"
                            </button>
                            <a
                                href="/bridge-helper"
                                target="_blank"
                                style="padding: 8px 16px; background: #007acc; color: white; border: 1px solid #007acc; border-radius: 4px; cursor: pointer; font-size: 0.9rem; line-height: 1.4; text-decoration: none; display: inline-block;">
                                "Download Helper"
                            </a>
                        </div>
                    </div>
                </div>
            </Show>

            // Bridge port picker dialog
            <Show when=move || !bridge_ports.get().is_empty() fallback=|| ()>
                <div style="position: fixed; top: 0; left: 0; width: 100vw; height: 100vh; background: rgba(0,0,0,0.7); z-index: 10000; display: flex; align-items: center; justify-content: center;">
                    <div style="background: #2a2a2a; border: 1px solid #555; border-radius: 8px; padding: 24px 32px; max-width: 480px; min-width: 320px; color: #eee; font-family: sans-serif;">
                        <h2 style="margin: 0 0 16px; font-size: 1.2rem;">"Select Serial Port"</h2>
                        {move || {
                            bridge_ports.get().into_iter().map(|(path, desc)| {
                                let path_click = path.clone();
                                view! {
                                    <button
                                        style="display: block; width: 100%; padding: 10px 16px; margin: 4px 0; background: #333; color: #eee; border: 1px solid #555; border-radius: 4px; cursor: pointer; text-align: left; font-size: 0.9rem;"
                                        on:click=move |_| set_bridge_port_pick.set(Some(path_click.clone()))>
                                        {desc}
                                    </button>
                                }
                            }).collect_view()
                        }}
                        <button
                            style="display: block; width: 100%; padding: 8px 16px; margin-top: 12px; background: #444; color: #ccc; border: 1px solid #666; border-radius: 4px; cursor: pointer; font-size: 0.9rem;"
                            on:click=move |_| set_bridge_port_pick.set(Some(String::new()))>
                            "Cancel"
                        </button>
                    </div>
                </div>
            </Show>

            <header style="padding: 10px; background: rgb(25, 25, 25); display: flex; align-items: center; gap: 10px; border-bottom: 1px solid rgb(45, 45, 45);">
                <h1 style="margin: 0; font-family: 'Impact', 'Arial Black', sans-serif; font-style: italic; font-size: 1.5rem; font-weight: normal; letter-spacing: 1px;">FutureTerm</h1>
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
