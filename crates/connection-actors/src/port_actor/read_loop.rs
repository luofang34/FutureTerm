use actor_protocol::SystemEvent;
use actor_runtime::{actor_debug, StateMessage};
use core_types::Transport;
use futures::{future::FutureExt, stream::StreamExt};
use futures_channel::mpsc;
use wasm_bindgen_futures::spawn_local;

use super::SendableTransport;

pub(super) fn spawn_read_loop(
    transport: SendableTransport,
    mut event_tx: mpsc::Sender<SystemEvent>,
    mut state_tx: mpsc::Sender<StateMessage>,
    mut shutdown_rx: mpsc::Receiver<()>,
    suppress_echo: bool,
    done_tx: futures_channel::oneshot::Sender<()>,
) {
    // WebSerialTransport implements Send/Sync for WASM (single-threaded)
    // SendableTransport is safe to move into spawn_local (same thread)
    spawn_local(async move {
        let mut check_suppress = suppress_echo;

        loop {
            // Create futures for reading and shutdown
            let read_fut = transport.read_chunk().fuse();
            let shutdown_fut = shutdown_rx.next().fuse();

            futures::pin_mut!(read_fut, shutdown_fut);

            let read_result = futures::select! {
                res = read_fut => Some(res),
                _ = shutdown_fut => None, // Shutdown signal
            };

            match read_result {
                Some(Ok((mut data, timestamp_us))) if !data.is_empty() => {
                    if check_suppress {
                        // Strip leading whitespace (CR, LF) which are likely the echo of our wakeup
                        let start = data
                            .iter()
                            .position(|&b| b != b'\r' && b != b'\n' && b != 0)
                            .unwrap_or(data.len());
                        if start > 0 {
                            actor_debug!("PortActor: Suppressed {} echo bytes", start);
                            data = data.split_off(start);
                        }
                        // Only disable check if we actually found data or stripped something?
                        // Actually, if we got a packet, that's the response. Turn off check.
                        check_suppress = false;
                    }

                    if !data.is_empty() {
                        let _ = event_tx.try_send(SystemEvent::DataReceived { data, timestamp_us });
                        let _ = event_tx.try_send(SystemEvent::RxActivity);
                    }
                }
                Some(Err(_)) => {
                    // Connection lost
                    let _ = state_tx.try_send(StateMessage::ConnectionLost);
                    let _ = event_tx.try_send(SystemEvent::Error {
                        message: "Connection lost".to_string(),
                    });
                    break; // Exit read loop
                }
                None => {
                    // Shutdown signal received
                    break;
                }
                _ => {} // Empty read is OK (timeout)
            }
        }

        // CRITICAL: Close the port when exiting loop
        // Try to unwrap the Rc to get exclusive ownership for explicit close
        match std::rc::Rc::try_unwrap(transport.0) {
            Ok(mut t) => {
                let _ = t.close().await;
                actor_debug!("Read loop: Port closed (exclusive ownership)");
            }
            Err(rc) => {
                // Cannot close explicitly - multiple references still exist
                // The Drop implementation will handle cleanup when the last reference is dropped
                actor_debug!(
                    "Read loop: Cannot force close - Rc still shared (strong_count={}). \
                     Port will be cleaned by Drop implementation.",
                    std::rc::Rc::strong_count(&rc)
                );
                drop(rc);
            }
        }

        // Signal completion to PortActor (allows handle_close to wait for cleanup)
        let _ = done_tx.send(());
        actor_debug!("Read loop: Cleanup complete, signaled done");
    });
}
