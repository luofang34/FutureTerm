mod bridge;
mod webserial;

pub use bridge::run_startup_precheck;

use crate::bridge_context::BridgeContext;
use crate::context::AppContext;
use leptos::*;
use wasm_bindgen_futures::spawn_local;

/// Top-level connect/disconnect handler.
///
/// Determines whether the current session should use the WebSerial API
/// (Chrome/Edge) or the WebSocket bridge (Safari/Firefox) and dispatches
/// to the appropriate sub-module.
pub fn on_connect(ctx: &AppContext, bctx: &BridgeContext, force_picker: bool) {
    let shift_held = force_picker;
    let manager = &ctx.manager;
    let current_state = manager.state.get();

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
        if bctx.active.get() {
            // Bridge disconnect - signal the read loop to stop
            // Set closing flag so connect flow waits for cleanup to finish
            bctx.closing.set(true);
            bctx.active.set(false);
            return;
        }

        // WebSerial disconnect
        #[cfg(debug_assertions)]
        web_sys::console::log_1(&"DEBUG: Executing disconnect logic".into());
        let manager_d = manager.clone();
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

    let current_baud = ctx.baud_rate.get_untracked();

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

    let manager = manager.clone();

    let bctx = bctx.clone();
    let framing = ctx.framing;

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
            bridge::connect(&manager, &window, shift_held, current_baud, &bctx).await;
            return;
        }

        webserial::connect(
            &manager,
            shift_held,
            current_baud,
            init_vid,
            init_pid,
            framing,
        )
        .await;
    });
}
