use core_types::Transport;
use leptos::*;
use wasm_bindgen::JsCast;

use crate::actor_bridge::ActorBridge;
use crate::bridge_context::BridgeContext;

/// Expected daemon version must match the daemon built with this release.
const EXPECTED_DAEMON_VERSION: &str = "0.3.2";

/// Launch the bridge daemon via URL scheme (futureterm://launch).
pub(super) fn launch_daemon_via_url_scheme(window: &web_sys::Window) {
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
pub(super) async fn check_daemon_version(
    manager: &ActorBridge,
    window: &web_sys::Window,
    ws_transport: &mut transport_websocket::WebSocketTransport,
    ws_url: &str,
    bctx: &BridgeContext,
) -> bool {
    let daemon_ver = ws_transport.daemon_version();
    match daemon_ver.as_deref() {
        None => {
            // Pre-Hello daemon (very old), no Shutdown support
            manager
                .set_status
                .set("Helper app outdated \u{2014} please download the latest version".into());
            bctx.set_show_install.set(true);
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
                bctx.set_show_install.set(true);
                return false;
            }
            true
        }
    }
}

/// Show all ports in picker for manual selection (triangle button).
///
/// Returns `Some(path)` on selection, `None` if cancelled or empty list.
pub(super) async fn select_port_manual(
    manager: &ActorBridge,
    deduped_ports: &[&transport_websocket::BridgePortInfo],
    bctx: &BridgeContext,
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

    bctx.set_ports.set(display);
    bctx.set_port_pick.set(None);
    manager.set_status.set("Select a serial port...".into());

    loop {
        if let Some(path) = bctx.port_pick.get_untracked() {
            bctx.set_ports.set(Vec::new());
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
pub(super) async fn select_port_auto(
    manager: &ActorBridge,
    deduped_ports: &[&transport_websocket::BridgePortInfo],
    bctx: &BridgeContext,
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

        bctx.set_ports.set(display);
        bctx.set_port_pick.set(None);
        manager.set_status.set("Select a serial port...".into());

        loop {
            if let Some(path) = bctx.port_pick.get_untracked() {
                bctx.set_ports.set(Vec::new());
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
