use actor_protocol::ConnectionState;
use core_types::Transport;
use leptos::*;

use crate::actor_bridge::ActorBridge;
use crate::bridge_context::BridgeContext;
use crate::protocol::UiToWorker;

/// Maximum number of chunks allowed in the bridge TX queue.
/// Prevents unbounded growth if the WebSocket stalls or the daemon is slow.
/// 1024 chunks × ~6 bytes each ≈ 6 KB — well within reason.
const MAX_TX_QUEUE: usize = 1024;

/// Main read/write loop with device-lost auto-reconnect.
pub(super) async fn read_loop(
    manager: &ActorBridge,
    ws_transport: &transport_websocket::WebSocketTransport,
    port_path: &str,
    final_baud: &mut u32,
    bctx: &BridgeContext,
) {
    'bridge: loop {
        // Inner read/write loop
        loop {
            // Check if bridge was deactivated (user disconnect)
            if !bctx.active.get() {
                break 'bridge;
            }

            // Apply pending baud rate change (set by reconfigure effect)
            {
                let pending = bctx.pending_baud.get();
                if pending > 0 {
                    bctx.pending_baud.set(0);
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
                    let mut queue = bctx.tx_queue.borrow_mut();
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
        if !bctx.active.get() {
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
            if !bctx.active.get() {
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
