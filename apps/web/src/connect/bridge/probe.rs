use core_types::Transport;
use leptos::*;

use crate::actor_bridge::ActorBridge;
use crate::bridge_context::BridgeContext;

/// Auto-probe baud rate via WebSocket bridge.
///
/// Tries common baud rates using set_config (no close/reopen needed),
/// scores received data, and returns the best match.
pub(super) async fn auto_probe(
    ws_transport: &transport_websocket::WebSocketTransport,
    manager: &ActorBridge,
    bctx: &BridgeContext,
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
        if bctx.closing.get() {
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
