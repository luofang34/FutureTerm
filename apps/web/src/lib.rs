use crate::protocol::UiToWorker;
use core_types::Transport;
use leptos::*;
use std::cell::Cell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::Worker;

// Actor system (replaces ConnectionManager)
mod actor_bridge;
mod actor_system;
use actor_bridge::ActorBridge;
use actor_protocol::ConnectionState;

mod context;
use context::create_app_context;

mod data_dispatch;

mod hex_view;
// mod mavlink_view; // Removed duplicate
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
    // Only extract what is actually used in lib.rs — the worker dispatch
    // signals are accessed directly via ctx inside data_dispatch.rs.
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
    let bridge_closing = ctx.bridge_closing.clone();
    let bridge_tx_queue = ctx.bridge_tx_queue.clone();
    let bridge_pending_baud = ctx.bridge_pending_baud.clone();
    let bridge_ports = ctx.bridge_ports;
    let set_bridge_ports = ctx.set_bridge_ports;
    let bridge_port_pick = ctx.bridge_port_pick;
    let set_bridge_port_pick = ctx.set_bridge_port_pick;
    let bridge_ready = ctx.bridge_ready;
    let set_bridge_ready = ctx.set_bridge_ready;

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
        spawn_local(async move {
            let ws_url = "wss://local.futureterm.app:9876";

            // 1. Quick probe — daemon may already be running.
            let mut ws = transport_websocket::WebSocketTransport::new();
            if ws.connect(ws_url).await.is_ok() {
                let _ = ws.close().await;
                set_bridge_ready.set(Some(true));
                return;
            }

            // 2. Daemon not running — try URL scheme launch.
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

            // 4. Helper not installed — show dialog and keep polling.
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

    // Transport removed
    let manager_con_main = manager.clone();
    // Use manager for disconnect
    let manager_disc = manager.clone();

    // Bridge mode clones - all clones must happen before any move closure
    let bridge_active_disc = bridge_active.clone();
    let bridge_closing_disc = bridge_closing.clone();
    let bridge_active_for_connect = bridge_active.clone();
    let bridge_closing_for_connect = bridge_closing.clone();
    let bridge_tx_queue_for_connect = bridge_tx_queue.clone();
    let bridge_pending_baud_for_connect = bridge_pending_baud.clone();
    let bridge_active_reconf = bridge_active.clone();
    let bridge_pending_baud_reconf = bridge_pending_baud.clone();
    let bridge_active_term = bridge_active.clone();
    let bridge_tx_queue_term = bridge_tx_queue.clone();

    let on_connect = move |force_picker: bool| {
        let shift_held = force_picker;
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

        // Allow disconnect if state allows it
        if current_state.can_disconnect() && !force_picker {
            // Check if we're in bridge mode
            if bridge_active_disc.get() {
                // Bridge disconnect - signal the read loop to stop
                // Set closing flag so connect flow waits for cleanup to finish
                bridge_closing_disc.set(true);
                bridge_active_disc.set(false);
                return;
            }

            // WebSerial disconnect
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

        // Bridge mode clones for the async block
        let bridge_active_connect = bridge_active_for_connect.clone();
        let bridge_closing_connect = bridge_closing_for_connect.clone();
        let bridge_tx_queue_connect = bridge_tx_queue_for_connect.clone();
        let bridge_pending_baud_connect = bridge_pending_baud_for_connect.clone();

        spawn_local(async move {
            let Some(window) = web_sys::window() else {
                manager
                    .set_status
                    .set("Error: window not available.".into());
                return;
            };
            let nav = window.navigator();
            let serial = nav.serial();

            // Phase 3: WebSocket fallback for Safari/Firefox
            if serial.is_undefined() {
                // Wait for any in-progress bridge cleanup to finish.
                // This prevents the race where Disconnect→Connect fires
                // before the old session's close_port() completes on the daemon,
                // causing "Device or resource busy" errors.
                if bridge_closing_connect.get() {
                    manager.set_status.set("Waiting for disconnect...".into());
                    let mut waited = 0u32;
                    while bridge_closing_connect.get() && waited < 3000 {
                        gloo_timers::future::TimeoutFuture::new(10).await;
                        waited += 10;
                    }
                }

                // If already in bridge mode, disconnect first (hot-swap / manual select)
                if bridge_active_connect.get() {
                    bridge_closing_connect.set(true);
                    bridge_active_connect.set(false);
                    // Wait for old read loop to exit and cleanup
                    let mut waited = 0u32;
                    while bridge_closing_connect.get() && waited < 3000 {
                        gloo_timers::future::TimeoutFuture::new(10).await;
                        waited += 10;
                    }
                }

                // WebSerial not available — use WebSocket bridge.
                //
                // The startup pre-check already tried to launch the helper
                // via URL scheme and is polling in the background.  If the
                // daemon is reachable (bridge_ready == Some(true)), connect
                // directly.  Otherwise wait for the pre-check to finish.
                let ws_url = "wss://local.futureterm.app:9876";

                let mut ws_transport = transport_websocket::WebSocketTransport::new();

                // Wait until the startup pre-check resolves (usually
                // instant — it runs at page load and finishes in <4 s).
                loop {
                    match bridge_ready.get() {
                        Some(_) => break,
                        None => gloo_timers::future::TimeoutFuture::new(100).await,
                    }
                }

                if bridge_ready.get() != Some(true) {
                    // Pre-check polling is already showing the install
                    // dialog and retrying.  Nothing more to do here.
                    return;
                }

                // Daemon is reachable — connect.
                manager
                    .set_status
                    .set("Connecting to bridge\u{2026}".into());
                if ws_transport.connect(ws_url).await.is_err() {
                    // Daemon went away between pre-check and now — retry
                    // via URL scheme + fast retries.
                    manager.set_status.set("Reconnecting\u{2026}".into());
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
                                let _ = window
                                    .set_timeout_with_callback_and_timeout_and_arguments_0(
                                        cleanup.as_ref().unchecked_ref(),
                                        1000,
                                    );
                                cleanup.forget();
                            }
                        }
                    }
                    let mut ok = false;
                    for &delay in &[500u32, 500, 1000, 1500] {
                        gloo_timers::future::TimeoutFuture::new(delay).await;
                        ws_transport = transport_websocket::WebSocketTransport::new();
                        if ws_transport.connect(ws_url).await.is_ok() {
                            ok = true;
                            break;
                        }
                    }
                    if !ok {
                        manager
                            .set_status
                            .set("Helper not available. Try again.".into());
                        set_bridge_ready.set(Some(false));
                        return;
                    }
                }

                // Check daemon version — restart if outdated.
                // Expected version must match the daemon built with this release.
                const EXPECTED_DAEMON_VERSION: &str = "0.3.2";

                let daemon_ver = ws_transport.daemon_version();
                match daemon_ver.as_deref() {
                    None => {
                        // Pre-Hello daemon (very old), no Shutdown support
                        manager.set_status.set(
                            "Helper app outdated \u{2014} please download the latest version"
                                .into(),
                        );
                        set_show_bridge_install.set(true);
                        return;
                    }
                    Some(v) if v == EXPECTED_DAEMON_VERSION => {
                        // Version matches, proceed normally
                    }
                    Some(_v) => {
                        // Version mismatch — try graceful shutdown + relaunch.
                        // Shutdown command was added in v0.2.0; older daemons
                        // will ignore the unknown message type harmlessly.
                        #[cfg(debug_assertions)]
                        web_sys::console::log_1(
                            &format!(
                                "Daemon version mismatch: got {}, expected {}. Restarting...",
                                _v, EXPECTED_DAEMON_VERSION
                            )
                            .into(),
                        );
                        manager.set_status.set("Updating helper app...".into());
                        let _ = ws_transport.send_shutdown();
                        let _ = ws_transport.close().await;
                        // Wait for old daemon to exit
                        gloo_timers::future::TimeoutFuture::new(600).await;

                        // Relaunch via URL scheme
                        let launch_url = "futureterm://launch?port=9876";
                        if let Some(doc) = window.document() {
                            if let Ok(iframe) = doc.create_element("iframe") {
                                let _ = iframe.set_attribute("style", "display:none");
                                let _ = iframe.set_attribute("src", launch_url);
                                if let Some(body) = doc.body() {
                                    let _ = body.append_child(&iframe);
                                    let body_clone = body.clone();
                                    let iframe_clone = iframe.clone();
                                    let cleanup = wasm_bindgen::closure::Closure::once(move || {
                                        let _ = body_clone.remove_child(&iframe_clone);
                                    });
                                    let _ = window
                                        .set_timeout_with_callback_and_timeout_and_arguments_0(
                                            cleanup.as_ref().unchecked_ref(),
                                            1000,
                                        );
                                    cleanup.forget();
                                }
                            }
                        }

                        // Retry connection to the newly launched daemon
                        let retry_delays_ms: &[u32] = &[500, 1000, 1500, 2000];
                        let mut restarted = false;
                        for &delay in retry_delays_ms.iter() {
                            gloo_timers::future::TimeoutFuture::new(delay).await;
                            ws_transport = transport_websocket::WebSocketTransport::new();
                            if ws_transport.connect(ws_url).await.is_ok() {
                                restarted = true;
                                break;
                            }
                        }

                        if !restarted {
                            manager.set_status.set(
                                "Helper app outdated \u{2014} please download the latest version"
                                    .into(),
                            );
                            set_show_bridge_install.set(true);
                            return;
                        }
                    }
                }

                // Bridge connected - list available serial ports
                manager.set_status.set("Listing serial ports...".into());

                let ports = match ws_transport.list_ports().await {
                    Ok(p) => p,
                    Err(e) => {
                        manager
                            .set_status
                            .set(format!("Failed to list ports: {}", e));
                        return;
                    }
                };

                // Deduplicate: macOS lists both /dev/cu.* and /dev/tty.* per device.
                // Prefer cu.* (calling unit) - tty.* blocks on DCD which breaks probing.
                let deduped_ports: Vec<_> = ports
                    .iter()
                    .filter(|p| {
                        if p.path.starts_with("/dev/tty.") {
                            // Skip tty.* if corresponding cu.* exists
                            let cu_path = p.path.replace("/dev/tty.", "/dev/cu.");
                            !ports.iter().any(|other| other.path == cu_path)
                        } else {
                            true
                        }
                    })
                    .collect();

                // ── Port Selection ──
                let port_path = if shift_held {
                    // Triangle button: show ALL ports in picker (manual selection)
                    let display: Vec<(String, String)> = deduped_ports
                        .iter()
                        .map(|p| {
                            let label = match (&p.product, &p.manufacturer) {
                                (Some(prod), Some(mfr)) => {
                                    format!("{} - {} ({})", prod, mfr, p.path)
                                }
                                (Some(prod), None) => format!("{} ({})", prod, p.path),
                                _ => p.path.clone(),
                            };
                            (p.path.clone(), label)
                        })
                        .collect();

                    if display.is_empty() {
                        manager.set_status.set("No serial ports found.".into());
                        return;
                    }

                    set_bridge_ports.set(display);
                    set_bridge_port_pick.set(None);
                    manager.set_status.set("Select a serial port...".into());

                    loop {
                        if let Some(path) = bridge_port_pick.get_untracked() {
                            set_bridge_ports.set(Vec::new());
                            if path.is_empty() {
                                manager
                                    .set_status
                                    .set("Cancelled — click Connect to try again".into());
                                return;
                            }
                            break path;
                        }
                        gloo_timers::future::TimeoutFuture::new(50).await;
                    }
                } else {
                    // Connect button: auto-select USB serial port
                    let usb_ports: Vec<_> = deduped_ports
                        .iter()
                        .filter(|p| p.port_type == "usb_serial")
                        .collect();

                    if usb_ports.len() == 1 {
                        // Single USB device - auto-select (like Chrome behavior)
                        usb_ports
                            .first()
                            .map(|p| p.path.clone())
                            .unwrap_or_default()
                    } else if usb_ports.len() > 1 {
                        // Multiple USB devices - show picker with USB ports only
                        let display: Vec<(String, String)> = usb_ports
                            .iter()
                            .map(|p| {
                                let label = match (&p.product, &p.manufacturer) {
                                    (Some(prod), Some(mfr)) => {
                                        format!("{} - {} ({})", prod, mfr, p.path)
                                    }
                                    (Some(prod), None) => format!("{} ({})", prod, p.path),
                                    _ => p.path.clone(),
                                };
                                (p.path.clone(), label)
                            })
                            .collect();

                        set_bridge_ports.set(display);
                        set_bridge_port_pick.set(None);
                        manager.set_status.set("Select a serial port...".into());

                        loop {
                            if let Some(path) = bridge_port_pick.get_untracked() {
                                set_bridge_ports.set(Vec::new());
                                if path.is_empty() {
                                    manager
                                        .set_status
                                        .set("Cancelled — click Connect to try again".into());
                                    return;
                                }
                                break path;
                            }
                            gloo_timers::future::TimeoutFuture::new(50).await;
                        }
                    } else {
                        // No USB devices found
                        manager
                            .set_status
                            .set("No USB serial devices found. Plug in a device and retry.".into());
                        return;
                    }
                };

                // ── Open Port ──
                // Initial baud rate (will be changed during probing if baud=0)
                let initial_baud = if current_baud == 0 {
                    115200
                } else {
                    current_baud
                };

                manager.set_status.set(format!("Opening {}...", port_path));

                if let Err(e) = ws_transport.open_port(&port_path, initial_baud).await {
                    manager
                        .set_status
                        .set(format!("Failed to open {}: {}", port_path, e));
                    return;
                }

                // ── Auto-Probe Baud Rate ──
                // Set bridge_active + Probing state so the Disconnect button
                // works during probe (same UX as WebSerial probing).
                let mut final_baud = if current_baud == 0 {
                    bridge_active_connect.set(true);
                    manager.set_connection_state(ConnectionState::Probing);

                    match bridge_auto_probe(&ws_transport, &manager, &bridge_closing_connect).await
                    {
                        Ok(baud) => baud,
                        Err(e) => {
                            // User cancelled or real failure
                            if bridge_closing_connect.get() {
                                // Clean up: close port, reset state
                                let _ = ws_transport.close_port().await;
                                let _ = ws_transport.close().await;
                                bridge_active_connect.set(false);
                                bridge_closing_connect.set(false);
                                manager.set_connection_state(ConnectionState::Disconnected);
                                manager.set_status.set("Disconnected".into());
                                return;
                            }
                            #[cfg(debug_assertions)]
                            web_sys::console::log_1(
                                &format!("Bridge auto-probe failed: {}", e).into(),
                            );
                            manager
                                .set_status
                                .set(format!("Auto-detect failed: {}. Using 115200.", e));
                            let _ = ws_transport.set_baud_rate(115200).await;
                            115200
                        }
                    }
                } else {
                    current_baud
                };

                // ── Connected ──
                manager
                    .set_status
                    .set(format!("Connected: {} @ {}", port_path, final_baud));
                manager.set_connection_state(ConnectionState::Connected);
                manager.send_worker_message(UiToWorker::Connect {
                    baud_rate: final_baud,
                });
                manager.set_detected_baud.set(final_baud);
                bridge_active_connect.set(true);

                #[cfg(debug_assertions)]
                web_sys::console::log_1(
                    &format!(
                        "Bridge: serial port {} opened at {} baud, starting read loop",
                        port_path, final_baud
                    )
                    .into(),
                );

                // Bridge read/write loop with auto-reconnect
                'bridge: loop {
                    // Inner read/write loop
                    loop {
                        // Check if bridge was deactivated (user disconnect)
                        if !bridge_active_connect.get() {
                            break 'bridge;
                        }

                        // Apply pending baud rate change (set by reconfigure effect)
                        {
                            let pending = bridge_pending_baud_connect.get();
                            if pending > 0 {
                                bridge_pending_baud_connect.set(0);
                                if ws_transport.set_baud_rate(pending).await.is_ok() {
                                    final_baud = pending;
                                    manager.set_detected_baud.set(final_baud);
                                    manager.send_worker_message(UiToWorker::Connect {
                                        baud_rate: final_baud,
                                    });
                                    manager.set_status.set(format!(
                                        "Reconfigured: {} @ {}",
                                        port_path, final_baud
                                    ));
                                }
                            }
                        }

                        // Drain TX queue and send to daemon
                        {
                            let tx_data: Vec<Vec<u8>> =
                                bridge_tx_queue_connect.borrow_mut().drain(..).collect();
                            if !tx_data.is_empty() {
                                #[cfg(debug_assertions)]
                                web_sys::console::log_1(
                                    &format!(
                                        "Bridge TX: sending {} chunks to daemon",
                                        tx_data.len()
                                    )
                                    .into(),
                                );
                                let mut sent_any = false;
                                for data in tx_data {
                                    if ws_transport.write(&data).await.is_err() {
                                        #[cfg(debug_assertions)]
                                        web_sys::console::error_1(
                                            &"Bridge TX: write to daemon failed".into(),
                                        );
                                        break;
                                    }
                                    sent_any = true;
                                }
                                if sent_any {
                                    manager.trigger_tx();
                                }
                            }
                        }

                        // Read serial data from bridge
                        match ws_transport.read_chunk().await {
                            Ok((data, ts)) if !data.is_empty() => {
                                manager.trigger_rx();
                                manager.send_worker_message(UiToWorker::IngestData {
                                    data,
                                    timestamp_us: ts,
                                });
                            }
                            Err(_e) => {
                                #[cfg(debug_assertions)]
                                web_sys::console::log_1(
                                    &format!("Bridge: port lost: {}", _e).into(),
                                );
                                break; // Exit inner loop, enter retry
                            }
                            _ => {}
                        }

                        // Small yield to prevent busy-spinning
                        gloo_timers::future::TimeoutFuture::new(5).await;
                    }

                    // Device lost - try to reconnect (same as WebSerial behavior)
                    if !bridge_active_connect.get() {
                        break 'bridge;
                    }

                    // Transition to DeviceLost state (triggers orange pulsing indicator)
                    manager.set_connection_state(ConnectionState::DeviceLost);
                    manager
                        .set_status
                        .set("Device lost. Reconnecting...".into());
                    let _ = ws_transport.close_port().await;

                    // Retry re-opening the same port with backoff
                    manager.set_connection_state(ConnectionState::AutoReconnecting);
                    let retry_delays_ms: &[u32] = &[500, 1000, 1500, 2000, 2000, 2000];
                    let mut reconnected = false;
                    for (i, &delay) in retry_delays_ms.iter().enumerate() {
                        if !bridge_active_connect.get() {
                            break; // User clicked disconnect
                        }
                        gloo_timers::future::TimeoutFuture::new(delay).await;

                        // Clear error state so open_port can work
                        ws_transport.clear_error();

                        if ws_transport.open_port(&port_path, final_baud).await.is_ok() {
                            reconnected = true;
                            manager.set_connection_state(ConnectionState::Connected);
                            manager
                                .set_status
                                .set(format!("Reconnected: {} @ {}", port_path, final_baud));
                            break;
                        }
                        manager.set_status.set(format!(
                            "Device lost. Retrying... ({}/{})",
                            i + 1,
                            retry_delays_ms.len()
                        ));
                    }

                    if !reconnected {
                        manager.set_status.set(
                            "Device not found after retries. Click Connect to try again.".into(),
                        );
                        break 'bridge; // Give up after all retries
                    }
                    // Reconnected - continue outer loop (resume read/write)
                }

                // Cleanup - close_port on daemon first, then WebSocket connection
                bridge_closing_connect.set(true);
                let _ = ws_transport.close_port().await;
                let _ = ws_transport.close().await;
                bridge_active_connect.set(false);
                manager.set_connection_state(ConnectionState::Disconnected);
                manager.set_status.set("Disconnected".into());
                // Signal that cleanup is complete - connect flow can proceed
                bridge_closing_connect.set(false);
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

                // Use the baud rate the user selected in the dropdown.
                // baud_rate=0 means "Auto Baudrate" -> actor system will probe.
                // Any non-zero value means the user explicitly chose a baud rate.
                let final_baud = current_baud;

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

                if final_baud == 0 || current_framing == "Auto" {
                    manager.set_status.set("Auto-Detecting Config...".into());

                    #[cfg(debug_assertions)]
                    web_sys::console::log_1(
                        &format!("Smart Port Check: VID={:?} PID={:?}", vid, pid).into(),
                    );
                }

                let manager_conn = manager.clone();
                spawn_local(async move {
                    manager_conn
                        .connect(port, final_baud, &current_framing)
                        .await;
                });
            }
        });
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

/// Auto-probe baud rate via WebSocket bridge.
///
/// Tries common baud rates using set_config (no close/reopen needed),
/// scores received data, and returns the best match.
async fn bridge_auto_probe(
    ws_transport: &transport_websocket::WebSocketTransport,
    manager: &ActorBridge,
    cancel: &Rc<Cell<bool>>,
) -> Result<u32, String> {
    use core_types::Transport;

    // Same candidates as Chrome prober (connection-actors/src/constants.rs)
    const BAUD_CANDIDATES: &[u32] = &[
        115200, 1500000, 1000000, 2000000, 921600, 57600, 460800, 230400, 38400, 19200, 9600,
    ];

    let mut best_baud = 115200u32;
    let mut best_score = 0.0_f64;
    let mut best_protocol: Option<&str> = None;
    // Preserve data collected at the winning baud rate so we can show it in the terminal
    // (mirrors WebSerial behavior where probe data is forwarded directly to the worker).
    let mut best_buffer: Vec<u8> = Vec::new();

    for &baud in BAUD_CANDIDATES {
        // Check cancellation at the top of each iteration
        if cancel.get() {
            return Err("Probe cancelled by user".into());
        }
        manager
            .set_status
            .set(format!("AUTO: Testing {} baud...", baud));

        // Change baud rate via bridge daemon (set_config)
        if ws_transport.set_baud_rate(baud).await.is_err() {
            continue;
        }

        // Drain stale data from previous baud rate.
        // Generous timing for FTDI chips with 16ms latency timer + WS round-trip.
        ws_transport.clear_rx_buffer();
        gloo_timers::future::TimeoutFuture::new(80).await;
        ws_transport.clear_rx_buffer();

        // Send Ctrl+C (0x03) then CR.
        // Ctrl+C terminates any stuck command (caused by garbage bytes from wrong-baud probes)
        // and returns to the shell prompt. CR then executes an empty command to get a fresh
        // prompt. The combined response (~50 bytes: "^C\r\n<prompt>\r\n<prompt>") is larger
        // than a bare CR response (~30 bytes), improving score reliability.
        let _ = ws_transport.write(b"\x03\r").await;

        // Wait to collect data at this baud rate.
        // Covers: WS round-trip (~10ms) + device response (~10-100ms) +
        // daemon read-task mutex cycle (~100ms) + WS back (~5ms).
        gloo_timers::future::TimeoutFuture::new(350).await;

        // Read all available data
        let mut buffer = Vec::new();
        loop {
            match ws_transport.read_chunk().await {
                Ok((data, _)) if !data.is_empty() => {
                    buffer.extend_from_slice(&data);
                    if buffer.len() > 200 {
                        break;
                    }
                }
                _ => break,
            }
        }

        // Retry once for slow devices (FTDI latency, long response time)
        if buffer.is_empty() {
            gloo_timers::future::TimeoutFuture::new(150).await;
            loop {
                match ws_transport.read_chunk().await {
                    Ok((data, _)) if !data.is_empty() => {
                        buffer.extend_from_slice(&data);
                        if buffer.len() > 200 {
                            break;
                        }
                    }
                    _ => break,
                }
            }
        }

        if buffer.is_empty() {
            #[cfg(debug_assertions)]
            web_sys::console::log_1(&format!("AUTO: {} baud - no data received", baud).into());
            continue;
        }

        // Score the data using the analysis crate
        let score_8n1 = analysis::calculate_score_8n1(&buffer) as f64;
        let score_mav = analysis::calculate_score_mavlink(&buffer) as f64;

        let (score, protocol) = if score_mav > 0.85 {
            (score_mav, Some("mavlink"))
        } else {
            (score_8n1, None)
        };

        #[cfg(debug_assertions)]
        web_sys::console::log_1(
            &format!(
                "AUTO: {} baud - {} bytes, score_8n1={:.2}, score_mav={:.2}",
                baud,
                buffer.len(),
                score_8n1,
                score_mav
            )
            .into(),
        );

        if score > best_score {
            best_score = score;
            best_baud = baud;
            best_protocol = protocol;
            best_buffer = buffer.clone();
        }

        // Early exit on high confidence (same thresholds as Chrome prober).
        // At high baud rates (>=1Mbps), a bash prompt is ~30 bytes — use a lower
        // min_bytes threshold so we exit early instead of continuing to test all bauds.
        let threshold = if baud >= 1_000_000 { 0.85 } else { 0.98 };
        let min_bytes = if baud >= 1_000_000 { 24 } else { 64 };
        if best_score > threshold && buffer.len() > min_bytes {
            #[cfg(debug_assertions)]
            web_sys::console::log_1(
                &format!(
                    "AUTO: Early exit - {} baud with score {:.2}",
                    best_baud, best_score
                )
                .into(),
            );
            break;
        }
    }

    if best_score < 0.30 {
        return Err("No valid signal detected at any baud rate".into());
    }

    // Set final baud rate
    ws_transport
        .set_baud_rate(best_baud)
        .await
        .map_err(|e| format!("Failed to set final baud rate: {}", e))?;

    // Forward the data collected at the winning baud rate to the terminal.
    // This mirrors WebSerial behavior: probe data is shown instead of discarded,
    // so the user sees the device prompt immediately after connection.
    //
    // Reuse trim_shell_artifacts() which strips leading CR/LF, ANSI escape
    // sequences, and literal "^C" echoes before the actual prompt text.
    // WebSerial uses the same function in state_actor::handle_probe_complete.
    {
        use connection_actors::data_processing::trim_shell_artifacts;
        let display_data = trim_shell_artifacts(&best_buffer);
        if !display_data.is_empty() {
            let ts_us = (js_sys::Date::now() * 1000.0) as u64;
            manager.send_worker_message(crate::protocol::UiToWorker::IngestData {
                data: display_data,
                timestamp_us: ts_us,
            });
        }
    }

    // Do NOT clear rx_buffer here. Any data that arrived since the probe read
    // (including additional prompt output) is valid and will be picked up by
    // the bridge read loop.

    let protocol_str = best_protocol.unwrap_or("text");
    manager.set_status.set(format!(
        "AUTO: {} baud (score: {:.2}, {})",
        best_baud, best_score, protocol_str
    ));

    // If MAVLink detected, switch decoder
    if best_protocol == Some("mavlink") {
        manager.set_decoder("mavlink".into());
    }

    Ok(best_baud)
}
