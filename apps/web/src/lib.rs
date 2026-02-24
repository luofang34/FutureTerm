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
mod dialogs;
mod header;

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

    // Local aliases for closures that capture individual Copy/Clone fields.
    // Header and dialog signals are now accessed via use_context in their
    // respective component modules (header.rs, dialogs.rs).
    let set_terminal_ready = ctx.set_terminal_ready;
    let show_bridge_install = ctx.show_bridge_install;
    let set_show_bridge_install = ctx.set_show_bridge_install;
    let view_mode = ctx.view_mode;
    let set_view_mode = ctx.set_view_mode;
    let connected = ctx.connected;
    let baud_rate = ctx.baud_rate;
    let framing = ctx.framing;
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
            <dialogs::BridgeInstallDialog />
            // Bridge port picker dialog
            <dialogs::BridgePortPicker on_connect=on_connect.clone() />

            <header::Header on_connect=on_connect.clone() />
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
