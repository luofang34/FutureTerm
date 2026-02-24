use crate::actor_bridge::ActorBridge;
use actor_protocol::ConnectionState;
use leptos::*;
use wasm_bindgen_futures::spawn_local;

/// WebSerial connection flow (Chrome/Edge).
///
/// Handles auto-port selection via VID/PID caching, manual port picker,
/// hot-swap detection, and delegation to the actor system for the actual
/// serial connection.
pub async fn connect(
    manager: &ActorBridge,
    shift_held: bool,
    current_baud: u32,
    init_vid: Option<u16>,
    init_pid: Option<u16>,
    framing: ReadSignal<String>,
) {
    let (last_vid, set_last_vid) = create_signal::<Option<u16>>(init_vid);
    let (last_pid, set_last_pid) = create_signal::<Option<u16>>(init_pid);

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
}
