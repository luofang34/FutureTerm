use crate::actor_bridge::ActorBridge;
use crate::context::AppContext;
use crate::protocol::UiToWorker;
use actor_protocol::ConnectionState;
use core_types::Transport;
use leptos::*;
use std::cell::Cell;
use std::rc::Rc;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;

/// Expected daemon version must match the daemon built with this release.
const EXPECTED_DAEMON_VERSION: &str = "0.3.2";

/// Maximum number of chunks allowed in the bridge TX queue.
/// Prevents unbounded growth if the WebSocket stalls or the daemon is slow.
/// 1024 chunks × ~6 bytes each ≈ 6 KB — well within reason.
const MAX_TX_QUEUE: usize = 1024;

/// Run startup pre-checks for bridge transport availability.
///
/// Detects whether the browser has WebSerial support.  If NOT (Safari/Firefox),
/// quick-probes the bridge daemon via WebSocket, attempts a URL-scheme launch
/// if the daemon is not running, and retries with increasing delays.  If the
/// daemon is found, sets `bridge_ready = Some(true)`.  If not found after
/// retries, shows the bridge install dialog and starts 5-minute polling.
pub fn run_startup_precheck(ctx: &AppContext) {
    let has_webserial = web_sys::window()
        .map(|w| !w.navigator().serial().is_undefined())
        .unwrap_or(false);

    if !has_webserial {
        let set_install = ctx.set_show_bridge_install;
        let set_bridge_ready = ctx.set_bridge_ready;
        let show_bridge_install = ctx.show_bridge_install;

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
#[allow(clippy::too_many_arguments)]
pub async fn connect(
    manager: &ActorBridge,
    window: &web_sys::Window,
    shift_held: bool,
    current_baud: u32,
    bridge_active: &Rc<Cell<bool>>,
    bridge_closing: &Rc<Cell<bool>>,
    bridge_tx_queue: &Rc<std::cell::RefCell<Vec<Vec<u8>>>>,
    bridge_pending_baud: &Rc<Cell<u32>>,
    bridge_ready: ReadSignal<Option<bool>>,
    set_bridge_ready: WriteSignal<Option<bool>>,
    set_show_bridge_install: WriteSignal<bool>,
    set_bridge_ports: WriteSignal<Vec<(String, String)>>,
    set_bridge_port_pick: WriteSignal<Option<String>>,
    bridge_port_pick: ReadSignal<Option<String>>,
) {
    // Clear stale TX data from any previous session.
    // Without this, leftover queue items would be sent to the new connection.
    bridge_tx_queue.borrow_mut().clear();

    // Wait for any in-progress bridge cleanup to finish.
    // This prevents the race where Disconnect->Connect fires
    // before the old session's close_port() completes on the daemon,
    // causing "Device or resource busy" errors.
    if bridge_closing.get() {
        manager.set_status.set("Waiting for disconnect...".into());
        let mut waited = 0u32;
        while bridge_closing.get() && waited < 3000 {
            gloo_timers::future::TimeoutFuture::new(10).await;
            waited += 10;
        }
    }

    // If already in bridge mode, disconnect first (hot-swap / manual select)
    if bridge_active.get() {
        bridge_closing.set(true);
        bridge_active.set(false);
        // Wait for old read loop to exit and cleanup
        let mut waited = 0u32;
        while bridge_closing.get() && waited < 3000 {
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
            set_bridge_ready.set(Some(false));
            return;
        }
    }

    // Check daemon version -- restart if outdated.
    if !check_daemon_version(
        manager,
        window,
        &mut ws_transport,
        ws_url,
        set_show_bridge_install,
    )
    .await
    {
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
        match select_port_manual(
            manager,
            &deduped_ports,
            set_bridge_ports,
            set_bridge_port_pick,
            bridge_port_pick,
        )
        .await
        {
            Some(path) => path,
            None => return,
        }
    } else {
        match select_port_auto(
            manager,
            &deduped_ports,
            set_bridge_ports,
            set_bridge_port_pick,
            bridge_port_pick,
        )
        .await
        {
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
        bridge_active.set(true);
        manager.set_connection_state(ConnectionState::Probing);

        match auto_probe(&ws_transport, manager, bridge_closing).await {
            Ok(baud) => baud,
            Err(e) => {
                // User cancelled or real failure
                if bridge_closing.get() {
                    // Clean up: close port, reset state
                    let _ = ws_transport.close_port().await;
                    let _ = ws_transport.close().await;
                    bridge_active.set(false);
                    bridge_closing.set(false);
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
    bridge_active.set(true);

    #[cfg(debug_assertions)]
    web_sys::console::log_1(
        &format!(
            "Bridge: serial port {} opened at {} baud, starting read loop",
            port_path, final_baud
        )
        .into(),
    );

    // Bridge read/write loop with auto-reconnect
    read_loop(
        manager,
        &ws_transport,
        &port_path,
        &mut final_baud,
        bridge_active,
        bridge_tx_queue,
        bridge_pending_baud,
    )
    .await;

    // Cleanup - close_port on daemon first, then WebSocket connection
    bridge_closing.set(true);
    let _ = ws_transport.close_port().await;
    let _ = ws_transport.close().await;
    bridge_active.set(false);
    manager.set_connection_state(ConnectionState::Disconnected);
    manager.set_status.set("Disconnected".into());
    // Signal that cleanup is complete - connect flow can proceed
    bridge_closing.set(false);
}

/// Main read/write loop with device-lost auto-reconnect.
async fn read_loop(
    manager: &ActorBridge,
    ws_transport: &transport_websocket::WebSocketTransport,
    port_path: &str,
    final_baud: &mut u32,
    bridge_active: &Rc<Cell<bool>>,
    bridge_tx_queue: &Rc<std::cell::RefCell<Vec<Vec<u8>>>>,
    bridge_pending_baud: &Rc<Cell<u32>>,
) {
    'bridge: loop {
        // Inner read/write loop
        loop {
            // Check if bridge was deactivated (user disconnect)
            if !bridge_active.get() {
                break 'bridge;
            }

            // Apply pending baud rate change (set by reconfigure effect)
            {
                let pending = bridge_pending_baud.get();
                if pending > 0 {
                    bridge_pending_baud.set(0);
                    if ws_transport.set_baud_rate(pending).await.is_ok() {
                        *final_baud = pending;
                        manager.set_detected_baud.set(*final_baud);
                        manager.send_worker_message(UiToWorker::Connect {
                            baud_rate: *final_baud,
                        });
                        manager
                            .set_status
                            .set(format!("Reconfigured: {} @ {}", port_path, final_baud));
                    }
                }
            }

            // Drain TX queue and send to daemon.
            // Cap enforcement: if the queue grew beyond MAX_TX_QUEUE (e.g.
            // during auto-reconnect when the drain loop was paused), drop
            // the oldest entries to bound memory usage.
            {
                let tx_data: Vec<Vec<u8>> = {
                    let mut queue = bridge_tx_queue.borrow_mut();
                    let overflow = queue.len().saturating_sub(MAX_TX_QUEUE);
                    if overflow > 0 {
                        queue.drain(..overflow);
                        web_sys::console::warn_1(
                            &format!(
                                "Bridge TX: queue overflow, dropped {} oldest chunks",
                                overflow
                            )
                            .into(),
                        );
                    }
                    queue.drain(..).collect()
                }; // borrow_mut dropped here, before any await
                if !tx_data.is_empty() {
                    #[cfg(debug_assertions)]
                    web_sys::console::log_1(
                        &format!("Bridge TX: sending {} chunks to daemon", tx_data.len()).into(),
                    );
                    let mut sent_any = false;
                    for data in tx_data {
                        if ws_transport.write(&data).await.is_err() {
                            web_sys::console::error_1(&"Bridge TX: write failed".into());
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
                    web_sys::console::log_1(&format!("Bridge: port lost: {}", _e).into());
                    break; // Exit inner loop, enter retry
                }
                _ => {}
            }

            // Small yield to prevent busy-spinning
            gloo_timers::future::TimeoutFuture::new(5).await;
        }

        // Device lost - try to reconnect (same as WebSerial behavior)
        if !bridge_active.get() {
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
            if !bridge_active.get() {
                break; // User clicked disconnect
            }
            gloo_timers::future::TimeoutFuture::new(delay).await;

            // Clear error state so open_port can work
            ws_transport.clear_error();

            if ws_transport.open_port(port_path, *final_baud).await.is_ok() {
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
            manager
                .set_status
                .set("Device not found after retries. Click Connect to try again.".into());
            break 'bridge; // Give up after all retries
        }
        // Reconnected - continue outer loop (resume read/write)
    }
}

/// Auto-probe baud rate via WebSocket bridge.
///
/// Tries common baud rates using set_config (no close/reopen needed),
/// scores received data, and returns the best match.
async fn auto_probe(
    ws_transport: &transport_websocket::WebSocketTransport,
    manager: &ActorBridge,
    cancel: &Rc<Cell<bool>>,
) -> Result<u32, String> {
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
        // At high baud rates (>=1Mbps), a bash prompt is ~30 bytes -- use a lower
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

/// Launch the bridge daemon via URL scheme (futureterm://launch).
fn launch_daemon_via_url_scheme(window: &web_sys::Window) {
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

/// Check daemon version and restart if outdated.
///
/// Returns `true` if the daemon is at the expected version (or was
/// successfully restarted to it), `false` if the user needs to
/// download a newer helper.
async fn check_daemon_version(
    manager: &ActorBridge,
    window: &web_sys::Window,
    ws_transport: &mut transport_websocket::WebSocketTransport,
    ws_url: &str,
    set_show_bridge_install: WriteSignal<bool>,
) -> bool {
    let daemon_ver = ws_transport.daemon_version();
    match daemon_ver.as_deref() {
        None => {
            // Pre-Hello daemon (very old), no Shutdown support
            manager
                .set_status
                .set("Helper app outdated \u{2014} please download the latest version".into());
            set_show_bridge_install.set(true);
            false
        }
        Some(v) if v == EXPECTED_DAEMON_VERSION => {
            // Version matches, proceed normally
            true
        }
        Some(_v) => {
            // Version mismatch -- try graceful shutdown + relaunch.
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
            launch_daemon_via_url_scheme(window);

            // Retry connection to the newly launched daemon
            let retry_delays_ms: &[u32] = &[500, 1000, 1500, 2000];
            let mut restarted = false;
            for &delay in retry_delays_ms.iter() {
                gloo_timers::future::TimeoutFuture::new(delay).await;
                *ws_transport = transport_websocket::WebSocketTransport::new();
                if ws_transport.connect(ws_url).await.is_ok() {
                    restarted = true;
                    break;
                }
            }

            if !restarted {
                manager
                    .set_status
                    .set("Helper app outdated \u{2014} please download the latest version".into());
                set_show_bridge_install.set(true);
                return false;
            }
            true
        }
    }
}

/// Show all ports in picker for manual selection (triangle button).
///
/// Returns `Some(path)` on selection, `None` if cancelled or empty list.
async fn select_port_manual(
    manager: &ActorBridge,
    deduped_ports: &[&transport_websocket::BridgePortInfo],
    set_bridge_ports: WriteSignal<Vec<(String, String)>>,
    set_bridge_port_pick: WriteSignal<Option<String>>,
    bridge_port_pick: ReadSignal<Option<String>>,
) -> Option<String> {
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
        return None;
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
                    .set("Cancelled \u{2014} click Connect to try again".into());
                return None;
            }
            return Some(path);
        }
        gloo_timers::future::TimeoutFuture::new(50).await;
    }
}

/// Auto-select USB serial port (connect button without shift).
///
/// If exactly one USB device is found, it is auto-selected.
/// If multiple are found, a picker is shown.
/// Returns `None` if no USB devices or cancelled.
async fn select_port_auto(
    manager: &ActorBridge,
    deduped_ports: &[&transport_websocket::BridgePortInfo],
    set_bridge_ports: WriteSignal<Vec<(String, String)>>,
    set_bridge_port_pick: WriteSignal<Option<String>>,
    bridge_port_pick: ReadSignal<Option<String>>,
) -> Option<String> {
    let usb_ports: Vec<_> = deduped_ports
        .iter()
        .filter(|p| p.port_type == "usb_serial")
        .collect();

    if usb_ports.len() == 1 {
        // Single USB device - auto-select (like Chrome behavior)
        return usb_ports.first().map(|p| p.path.clone());
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
                        .set("Cancelled \u{2014} click Connect to try again".into());
                    return None;
                }
                return Some(path);
            }
            gloo_timers::future::TimeoutFuture::new(50).await;
        }
    }

    // No USB devices found
    manager
        .set_status
        .set("No USB serial devices found. Plug in a device and retry.".into());
    None
}
