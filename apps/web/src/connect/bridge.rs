use crate::actor_bridge::ActorBridge;
use crate::bridge_context::BridgeContext;
use crate::protocol::UiToWorker;
use actor_protocol::ConnectionState;
use core_types::Transport;
use leptos::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;

mod helpers;
mod probe;
mod read_loop;

use helpers::{
    check_daemon_version, launch_daemon_via_url_scheme, select_port_auto, select_port_manual,
};

/// Run startup pre-checks for bridge transport availability.
///
/// Detects whether the browser has WebSerial support.  If NOT (Safari/Firefox),
/// quick-probes the bridge daemon via WebSocket, attempts a URL-scheme launch
/// if the daemon is not running, and retries with increasing delays.  If the
/// daemon is found, sets `bridge_ready = Some(true)`.  If not found after
/// retries, shows the bridge install dialog and starts 5-minute polling.
pub fn run_startup_precheck(bctx: &BridgeContext) {
    let has_webserial = web_sys::window()
        .map(|w| !w.navigator().serial().is_undefined())
        .unwrap_or(false);

    if !has_webserial {
        let set_install = bctx.set_show_install;
        let set_bridge_ready = bctx.set_ready;
        let show_bridge_install = bctx.show_install;

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
}

/// Bridge connection flow (Safari/Firefox via WebSocket daemon).
///
/// Handles daemon version checking, port listing/selection, serial port
/// opening, auto-baud probing, the main read/write loop, and device-lost
/// auto-reconnect.
pub async fn connect(
    manager: &ActorBridge,
    window: &web_sys::Window,
    shift_held: bool,
    current_baud: u32,
    bctx: &BridgeContext,
) {
    // Clear stale TX data from any previous session.
    // Without this, leftover queue items would be sent to the new connection.
    bctx.tx_queue.borrow_mut().clear();

    // Wait for any in-progress bridge cleanup to finish.
    // This prevents the race where Disconnect->Connect fires
    // before the old session's close_port() completes on the daemon,
    // causing "Device or resource busy" errors.
    if bctx.closing.get() {
        manager.set_status.set("Waiting for disconnect...".into());
        let mut waited = 0u32;
        while bctx.closing.get() && waited < 3000 {
            gloo_timers::future::TimeoutFuture::new(10).await;
            waited += 10;
        }
    }

    // If already in bridge mode, disconnect first (hot-swap / manual select)
    if bctx.active.get() {
        bctx.closing.set(true);
        bctx.active.set(false);
        // Wait for old read loop to exit and cleanup
        let mut waited = 0u32;
        while bctx.closing.get() && waited < 3000 {
            gloo_timers::future::TimeoutFuture::new(10).await;
            waited += 10;
        }
    }

    // WebSerial not available -- use WebSocket bridge.
    //
    // The startup pre-check already tried to launch the helper
    // via URL scheme and is polling in the background.  If the
    // daemon is reachable (bridge_ready == Some(true)), connect
    // directly.  Otherwise wait for the pre-check to finish.
    let ws_url = "wss://local.futureterm.app:9876";

    let mut ws_transport = transport_websocket::WebSocketTransport::new();

    // Wait until the startup pre-check resolves (usually
    // instant -- it runs at page load and finishes in <4 s).
    loop {
        match bctx.ready.get() {
            Some(_) => break,
            None => gloo_timers::future::TimeoutFuture::new(100).await,
        }
    }

    if bctx.ready.get() != Some(true) {
        // Pre-check polling is already showing the install
        // dialog and retrying.  Nothing more to do here.
        return;
    }

    // Daemon is reachable -- connect.
    manager
        .set_status
        .set("Connecting to bridge\u{2026}".into());
    if ws_transport.connect(ws_url).await.is_err() {
        // Daemon went away between pre-check and now -- retry
        // via URL scheme + fast retries.
        manager.set_status.set("Reconnecting\u{2026}".into());
        launch_daemon_via_url_scheme(window);

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
            bctx.set_ready.set(Some(false));
            return;
        }
    }

    // Check daemon version -- restart if outdated.
    if !check_daemon_version(manager, window, &mut ws_transport, ws_url, bctx).await {
        return;
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

    // -- Port Selection --
    let port_path = if shift_held {
        match select_port_manual(manager, &deduped_ports, bctx).await {
            Some(path) => path,
            None => return,
        }
    } else {
        match select_port_auto(manager, &deduped_ports, bctx).await {
            Some(path) => path,
            None => return,
        }
    };

    // -- Open Port --
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

    // -- Auto-Probe Baud Rate --
    // Set bridge_active + Probing state so the Disconnect button
    // works during probe (same UX as WebSerial probing).
    let mut final_baud = if current_baud == 0 {
        bctx.active.set(true);
        manager.set_connection_state(ConnectionState::Probing);

        match probe::auto_probe(&ws_transport, manager, bctx).await {
            Ok(baud) => baud,
            Err(e) => {
                // User cancelled or real failure
                if bctx.closing.get() {
                    // Clean up: close port, reset state
                    let _ = ws_transport.close_port().await;
                    let _ = ws_transport.close().await;
                    bctx.active.set(false);
                    bctx.closing.set(false);
                    manager.set_connection_state(ConnectionState::Disconnected);
                    manager.set_status.set("Disconnected".into());
                    return;
                }
                #[cfg(debug_assertions)]
                web_sys::console::log_1(&format!("Bridge auto-probe failed: {}", e).into());
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

    // -- Connected --
    manager
        .set_status
        .set(format!("Connected: {} @ {}", port_path, final_baud));
    manager.set_connection_state(ConnectionState::Connected);
    manager.send_worker_message(UiToWorker::Connect {
        baud_rate: final_baud,
    });
    manager.set_detected_baud.set(final_baud);
    bctx.active.set(true);

    #[cfg(debug_assertions)]
    web_sys::console::log_1(
        &format!(
            "Bridge: serial port {} opened at {} baud, starting read loop",
            port_path, final_baud
        )
        .into(),
    );

    // Bridge read/write loop with auto-reconnect
    read_loop::read_loop(manager, &ws_transport, &port_path, &mut final_baud, bctx).await;

    // Cleanup - close_port on daemon first, then WebSocket connection
    bctx.closing.set(true);
    let _ = ws_transport.close_port().await;
    let _ = ws_transport.close().await;
    bctx.active.set(false);
    manager.set_connection_state(ConnectionState::Disconnected);
    manager.set_status.set("Disconnected".into());
    // Signal that cleanup is complete - connect flow can proceed
    bctx.closing.set(false);
}
