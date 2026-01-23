use super::prober::detect_config;
use super::prober::smart_probe_framing;
use super::types::{
    parse_framing, ConnectionCommand, ConnectionHandle, ConnectionManager, ConnectionSnapshot,
    ConnectionState,
};
use crate::protocol::UiToWorker;
use core_types::SerialConfig;
use core_types::Transport;
use futures::select;
use futures::stream::StreamExt;
use futures::FutureExt;
use futures_channel::{mpsc, oneshot};
use leptos::*;
use std::sync::Arc;
use transport_webserial::WebSerialTransport;
use wasm_bindgen_futures::spawn_local;

// Constants
const DISCONNECT_COMPLETION_TIMEOUT_MS: i32 = 2000;

impl ConnectionManager {
    // Internal connect implementation
    pub async fn connect_impl(
        &self,
        port: web_sys::SerialPort,
        baud: u32,
        framing: &str,
        initial_buffer: Option<Vec<u8>>,
    ) -> Result<(), String> {
        let (d, p, s) = parse_framing(framing);

        let cfg = SerialConfig {
            baud_rate: baud,
            data_bits: d,
            parity: p,
            stop_bits: s,
            flow_control: "none".into(),
        };

        let mut t = WebSerialTransport::new();

        // Retry Loop for "Port already open" race condition
        let mut attempts = 0;
        let result = loop {
            match t.open(port.clone(), cfg.clone()).await {
                Ok(_) => {
                    // CREATE channel for commands
                    let (cmd_tx, cmd_rx) = mpsc::unbounded();
                    let (completion_tx, completion_rx) = oneshot::channel();

                    *self.connection_handle.borrow_mut() = Some(ConnectionHandle {
                        cmd_tx: cmd_tx.clone(),
                        completion_rx,
                    });
                    *self.active_port.borrow_mut() = Some(port.clone());

                    // Save VID/PID for auto-reconnect
                    let info = port.get_info();
                    let vid = js_sys::Reflect::get(&info, &"usbVendorId".into())
                        .ok()
                        .and_then(|v| v.as_f64())
                        .map(|v| v as u16);
                    let pid = js_sys::Reflect::get(&info, &"usbProductId".into())
                        .ok()
                        .and_then(|v| v.as_f64())
                        .map(|v| v as u16);

                    if let Some(set_vid) = *self.set_last_vid.borrow() {
                        set_vid.set(vid);
                    }
                    if let Some(set_pid) = *self.set_last_pid.borrow() {
                        set_pid.set(pid);
                    }

                    web_sys::console::log_1(
                        &format!(
                            "DEBUG: Saved VID/PID for auto-reconnect: {:?}/{:?}",
                            vid, pid
                        )
                        .into(),
                    );

                    // Transition from Connecting to Connected
                    self.set_state.set(ConnectionState::Connected); // Update signal
                                                                    // We don't need transition_to here because atomic state connection logic handles the lock?
                                                                    // Wait, connect() wrapper handles the lock.
                                                                    // So here we assume lock is held.

                    // Update detected config for UI
                    self.set_detected_baud.set(baud);
                    self.set_detected_framing.set(framing.to_string());

                    // Spawn read loop
                    self.spawn_read_loop(t, cmd_rx, completion_tx);

                    // Notify Worker
                    self.send_worker_config(baud);

                    // Replay Initial Buffer
                    if let Some(buf) = initial_buffer {
                        if !buf.is_empty() {
                            let start_idx = buf
                                .iter()
                                .position(|&x| x != b'\r' && x != b'\n')
                                .unwrap_or(0);
                            let clean_buf = if let Some(slice) = buf.get(start_idx..) {
                                slice.to_vec()
                            } else {
                                Vec::new()
                            };

                            if !clean_buf.is_empty() {
                                if let Some(w) = self.worker.get_untracked() {
                                    let msg = UiToWorker::IngestData {
                                        data: clean_buf,
                                        timestamp_us: (js_sys::Date::now() * 1000.0) as u64,
                                    };
                                    if let Ok(cmd_val) = serde_wasm_bindgen::to_value(&msg) {
                                        let _ = w.post_message(&cmd_val);
                                    }
                                }
                            }
                        }
                    }

                    break Ok(());
                }
                Err(e) => {
                    let err_str = format!("{:?}", e);
                    if (err_str.contains("already open") || err_str.contains("InvalidStateError"))
                        && attempts < 10
                    {
                        attempts += 1;
                        web_sys::console::warn_1(
                            &format!(
                                "Connection blocked by busy port. Retrying ({}/10)...",
                                attempts
                            )
                            .into(),
                        );
                        let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(
                            &mut |r, _| {
                                if let Some(window) = web_sys::window() {
                                    let _ = window
                                        .set_timeout_with_callback_and_timeout_and_arguments_0(
                                            &r, 200,
                                        );
                                }
                            },
                        ))
                        .await;
                        continue;
                    }
                    self.set_status.set(format!("Connection Failed: {:?}", e));
                    break Err(format!("{:?}", e));
                }
            }
        };
        result
    }

    pub fn spawn_read_loop(
        &self,
        mut transport: WebSerialTransport,
        mut cmd_rx: mpsc::UnboundedReceiver<ConnectionCommand>,
        completion_tx: oneshot::Sender<()>,
    ) {
        let worker_signal = self.worker;
        let manager = self.clone();

        spawn_local(async move {
            web_sys::console::log_1(&"DEBUG: Read Loop STARTED".into());

            loop {
                select! {
                    cmd = cmd_rx.next() => {
                        match cmd {
                            Some(ConnectionCommand::Stop) => {
                                web_sys::console::log_1(&"DEBUG: Read Loop received STOP".into());
                                break;
                            }
                            Some(ConnectionCommand::Write { data, response }) => {
                                let result = transport.write(&data).await
                                    .map_err(|e| format!("TX Error: {:?}", e));
                                let is_ok = result.is_ok();
                                let _ = response.send(result);
                                if is_ok {
                                    manager.trigger_tx();
                                }
                            }
                            None => break, // Channel closed
                        }
                    }

                    read_result = transport.read_chunk().fuse() => {
                        match read_result {
                            Ok((chunk, ts)) if !chunk.is_empty() => {
                                if let Some(w) = worker_signal.get_untracked() {
                                    let msg = UiToWorker::IngestData {
                                        data: chunk,
                                        timestamp_us: ts,
                                    };
                                    if let Ok(cmd_val) = serde_wasm_bindgen::to_value(&msg) {
                                        let _ = w.post_message(&cmd_val);
                                    }
                                }
                                manager.trigger_rx();
                            }
                            Err(e) => {
                                web_sys::console::log_1(
                                    &format!("DEBUG: Read Loop - Read Error: {:?}", e).into()
                                );
                                break;
                            }
                            _ => {
                                let _ = wasm_bindgen_futures::JsFuture::from(
                                    js_sys::Promise::resolve(&wasm_bindgen::JsValue::UNDEFINED),
                                )
                                .await;
                            }
                        }
                    }
                }
            }

            web_sys::console::log_1(&"DEBUG: Read Loop EXITED".into());
            let _ = transport.close().await;
            web_sys::console::log_1(&"DEBUG: Transport closed".into());

            let current_state = manager.state.get_untracked();
            if current_state == ConnectionState::Connected {
                manager.transition_to(ConnectionState::DeviceLost);
            }

            let _ = completion_tx.send(());
        });
    }

    pub async fn disconnect(&self) -> bool {
        let current_state = self.atomic_state.get();
        if !current_state.can_disconnect() {
            return false;
        }

        if self
            .atomic_state
            .try_transition(current_state, ConnectionState::Disconnecting)
            .is_err()
        {
            return false;
        }

        let atomic_state = Arc::clone(&self.atomic_state);
        let _guard = scopeguard::guard((), move |_| {
            atomic_state.unlock_and_set(ConnectionState::Disconnected);
        });

        self.user_initiated_disconnect.set(true);

        // CRITICAL: Always update signal to match atomic state
        // This keeps atomic_state and Leptos signal synchronized
        //
        // Background: try_transition() at line 248 updated atomic_state
        // to Disconnecting, but did NOT update the Leptos signal.
        //
        // If we skip this call, transition_to(Disconnected) at line 294
        // will validate using stale signal state, causing panic.
        self.transition_to(ConnectionState::Disconnecting);

        let handle = self.connection_handle.borrow_mut().take();
        if let Some(h) = handle {
            let _ = h.cmd_tx.unbounded_send(ConnectionCommand::Stop);
            let timeout_promise = js_sys::Promise::new(&mut |resolve, _| {
                if let Some(window) = web_sys::window() {
                    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                        &resolve,
                        DISCONNECT_COMPLETION_TIMEOUT_MS,
                    );
                }
            });
            let completion_future = h.completion_rx;
            let timeout_signal = wasm_bindgen_futures::JsFuture::from(timeout_promise);

            futures::select! {
                _ = completion_future.fuse() => {}
                _ = timeout_signal.fuse() => {
                    web_sys::console::warn_1(&"Read loop did not complete in time".into());
                }
            }
        }

        self.active_port.borrow_mut().take();
        self.clear_auto_reconnect_device();
        self.transition_to(ConnectionState::Disconnected);
        self.user_initiated_disconnect.set(false);

        drop(_guard);
        self.atomic_state
            .unlock_and_set(ConnectionState::Disconnected);

        true
    }

    fn capture_state(&self) -> ConnectionSnapshot {
        ConnectionSnapshot {
            baud: self.detected_baud.get_untracked(),
            framing: self.detected_framing.get_untracked(),
            decoder_id: self.decoder_id.get_untracked(),
        }
    }

    async fn restore_ui_state(&self, snapshot: ConnectionSnapshot) {
        self.set_detected_baud.set(snapshot.baud);
        self.set_detected_framing.set(snapshot.framing);
        self.set_decoder(snapshot.decoder_id);
    }

    pub async fn reconfigure(&self, baud: u32, framing: &str) {
        if self.state.get_untracked() != ConnectionState::Connected {
            return;
        }

        let snapshot = self.capture_state();
        let port_opt = self.active_port.borrow().clone();

        if !self.disconnect().await {
            return;
        }

        if let Some(port) = port_opt {
            let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(&mut |r, _| {
                if let Some(window) = web_sys::window() {
                    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&r, 100);
                }
            }))
            .await;

            let (final_baud, final_framing, initial_buf, proto) = if baud == 0 {
                let cached_auto = *self.last_auto_baud.borrow();
                if let Some(cached) = cached_auto {
                    let effective_framing = if framing == "Auto" { "8N1" } else { framing };
                    (cached, effective_framing.to_string(), None, None)
                } else {
                    let (b, f, buf, proto) = detect_config(
                        port.clone(),
                        framing,
                        self.set_status,
                        self.probing_interrupted.clone(),
                        self.last_auto_baud.clone(),
                    )
                    .await;
                    if self.active_port.borrow().is_none() {
                        self.transition_to(ConnectionState::Disconnected);
                        return;
                    }
                    (b, f, Some(buf), proto)
                }
            } else if framing == "Auto" {
                let (detect_f, buf, proto) =
                    smart_probe_framing(port.clone(), baud, self.set_status).await;
                if let Some(p) = proto.clone() {
                    self.set_decoder(p);
                }
                (baud, detect_f, Some(buf), proto)
            } else {
                (baud, framing.to_string(), None, None)
            };

            if let Some(p) = proto {
                self.set_decoder(p);
            }

            match self
                .connect_impl(port, final_baud, &final_framing, initial_buf)
                .await
            {
                Ok(_) => {
                    self.set_status.set("Reconfigured".into());
                }
                Err(e) => {
                    web_sys::console::error_1(&format!("Reconfigure failed: {}", e).into());
                    self.restore_ui_state(snapshot).await;
                    self.transition_to(ConnectionState::Disconnected);
                    self.set_status.set(format!("Reconfigure failed: {}", e));
                }
            }
        } else {
            self.transition_to(ConnectionState::Disconnected);
        }
    }
}
