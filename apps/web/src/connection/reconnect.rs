use super::types::{ConnectionManager, ConnectionState};
use leptos::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;

const AUTO_RECONNECT_RETRY_INTERVAL_MS: i32 = 50;
const AUTO_RECONNECT_MAX_RETRIES: usize = 200;

impl ConnectionManager {
    // --- Auto-Reconnect Logic ---
    #[allow(clippy::too_many_arguments)]
    pub fn setup_auto_reconnect(
        &self,
        last_vid: Signal<Option<u16>>,
        last_pid: Signal<Option<u16>>,
        set_last_vid: WriteSignal<Option<u16>>,
        set_last_pid: WriteSignal<Option<u16>>,
        baud_signal: Signal<u32>,
        detected_baud: Signal<u32>,
        framing_signal: Signal<String>,
    ) {
        *self.set_last_vid.borrow_mut() = Some(set_last_vid);
        *self.set_last_pid.borrow_mut() = Some(set_last_pid);

        let manager_conn = self.clone();
        let manager_disc = self.clone();

        let on_connect_closure = Closure::wrap(Box::new(move |_e: web_sys::Event| {
            let _t_onconnect = js_sys::Date::now();

            // OPTIMIZATION: Ignore auto-reconnect if manual connection already in progress
            let current_state = manager_conn.state.get_untracked();
            if matches!(
                current_state,
                ConnectionState::Probing | ConnectionState::Connecting
            ) {
                return;
            }

            let vid_opt = last_vid.get_untracked();
            let pid_opt = last_pid.get_untracked();

            if let (Some(target_vid), Some(target_pid)) = (vid_opt, pid_opt) {
                let manager_conn = manager_conn.clone();
                spawn_local(async move {
                    let _t0 = js_sys::Date::now();
                    let Some(window) = web_sys::window() else {
                        return;
                    };
                    let nav = window.navigator();
                    let serial = nav.serial();
                    let promise = serial.get_ports();

                    if let Ok(val) = wasm_bindgen_futures::JsFuture::from(promise).await {
                        let ports: js_sys::Array = val.unchecked_into();
                        for i in 0..ports.length() {
                            let p: web_sys::SerialPort = ports.get(i).unchecked_into();
                            let info = p.get_info();
                            let vid = js_sys::Reflect::get(&info, &"usbVendorId".into())
                                .ok()
                                .and_then(|v| v.as_f64())
                                .map(|v| v as u16);
                            let pid = js_sys::Reflect::get(&info, &"usbProductId".into())
                                .ok()
                                .and_then(|v| v.as_f64())
                                .map(|v| v as u16);

                            if vid == Some(target_vid) && pid == Some(target_pid) {
                                // Match found
                                manager_conn.trigger_auto_reconnect(
                                    p,
                                    baud_signal,
                                    detected_baud,
                                    framing_signal,
                                );
                                return; // Stop checking
                            }
                        }

                        manager_conn.transition_to(ConnectionState::AutoReconnecting);

                        // Retry loop logic
                        for _retry_attempt in 1..=AUTO_RECONNECT_MAX_RETRIES {
                            if manager_conn.atomic_state.is_locked() {
                                let current_state = manager_conn.state.get_untracked();
                                if current_state == ConnectionState::AutoReconnecting {
                                    manager_conn.transition_to(ConnectionState::DeviceLost);
                                }
                                return;
                            }

                            let user_canceled = manager_conn.user_initiated_disconnect.get();
                            let current_state = manager_conn.state.get_untracked();

                            if user_canceled || current_state == ConnectionState::Disconnected {
                                if current_state != ConnectionState::Disconnected {
                                    manager_conn.transition_to(ConnectionState::Disconnected);
                                }
                                manager_conn.clear_auto_reconnect_device();
                                return;
                            }

                            // Wait
                            let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(
                                &mut |r, _| {
                                    if let Some(window) = web_sys::window() {
                                        let _ = window
                                            .set_timeout_with_callback_and_timeout_and_arguments_0(
                                                &r,
                                                AUTO_RECONNECT_RETRY_INTERVAL_MS,
                                            );
                                    }
                                },
                            ))
                            .await;

                            // Retry getPorts
                            let promise_retry = serial.get_ports();
                            if let Ok(val_retry) =
                                wasm_bindgen_futures::JsFuture::from(promise_retry).await
                            {
                                let ports_retry: js_sys::Array = val_retry.unchecked_into();
                                for i in 0..ports_retry.length() {
                                    let p: web_sys::SerialPort =
                                        ports_retry.get(i).unchecked_into();
                                    let info = p.get_info();
                                    let vid = js_sys::Reflect::get(&info, &"usbVendorId".into())
                                        .ok()
                                        .and_then(|v| v.as_f64())
                                        .map(|v| v as u16);
                                    let pid = js_sys::Reflect::get(&info, &"usbProductId".into())
                                        .ok()
                                        .and_then(|v| v.as_f64())
                                        .map(|v| v as u16);

                                    if vid == Some(target_vid) && pid == Some(target_pid) {
                                        // Match found in retry
                                        manager_conn.trigger_auto_reconnect(
                                            p,
                                            baud_signal,
                                            detected_baud,
                                            framing_signal,
                                        );
                                        return;
                                    }
                                }
                            }
                        }

                        manager_conn.transition_to(ConnectionState::DeviceLost);
                    }
                });
            }
        }) as Box<dyn FnMut(_)>);

        let on_disconnect_closure = Closure::wrap(Box::new(move |_e: web_sys::Event| {
            let current_state = manager_disc.state.get_untracked();
            if current_state == ConnectionState::Reconfiguring {
                return;
            }
            if current_state == ConnectionState::Probing {
                manager_disc.probing_interrupted.set(true);
                return;
            }
            if current_state == ConnectionState::Disconnected {
                return;
            }

            let user_initiated = manager_disc.user_initiated_disconnect.get();
            if user_initiated {
                // User clicked disconnect. The disconnect() method is running and owns the lifecycle.
                // We should NOT interfere or try to transition state here.
            } else {
                manager_disc.transition_to(ConnectionState::DeviceLost);
            }
        }) as Box<dyn FnMut(_)>);

        let Some(window) = web_sys::window() else {
            return;
        };
        let nav = window.navigator();
        let serial = nav.serial();

        if !serial.is_undefined() {
            serial.set_onconnect(Some(on_connect_closure.as_ref().unchecked_ref()));
            serial.set_ondisconnect(Some(on_disconnect_closure.as_ref().unchecked_ref()));
            let _ = self
                .event_closures
                .borrow_mut()
                .replace((on_connect_closure, on_disconnect_closure));
        }
    }

    fn trigger_auto_reconnect(
        &self,
        port: web_sys::SerialPort,
        baud_signal: Signal<u32>,
        detected_baud: Signal<u32>,
        framing_signal: Signal<String>,
    ) {
        self.transition_to(ConnectionState::AutoReconnecting);

        let (target_baud, final_framing_str) = Self::calculate_reconnect_target(
            baud_signal.get_untracked(),
            detected_baud.get_untracked(),
            &framing_signal.get_untracked(),
        );

        let manager = self.clone();
        spawn_local(async move {
            Self::execute_reconnect(manager, port, target_baud, final_framing_str).await;
        });
    }

    async fn execute_reconnect(
        manager: ConnectionManager,
        port: web_sys::SerialPort,
        target_baud: u32,
        framing: String,
    ) {
        if manager.atomic_state.is_locked() {
            return;
        }

        // Force disconnect cleanup
        if !manager.disconnect().await {
            return;
        }

        manager.user_initiated_disconnect.set(false);

        let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(&mut |r, _| {
            if let Some(window) = web_sys::window() {
                let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&r, 100);
            }
        }))
        .await;

        if let Err(e) = manager.connect(port, target_baud, &framing).await {
            web_sys::console::log_1(&format!("Auto-reconnect failed: {}", e).into());
        } else {
            manager.set_status.set("Restored Connection".into());
        }
    }
    fn calculate_reconnect_target(
        user_pref_baud: u32,
        last_known_baud: u32,
        current_framing: &str,
    ) -> (u32, String) {
        let target_baud = if user_pref_baud == 0 && last_known_baud > 0 {
            last_known_baud
        } else if user_pref_baud == 0 {
            115200
        } else {
            user_pref_baud
        };

        let final_framing = if current_framing == "Auto" {
            "8N1".to_string()
        } else {
            current_framing.to_string()
        };

        (target_baud, final_framing)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_calculate_reconnect_target() {
        // Case 1: All zeroes
        let (baud, framing) = ConnectionManager::calculate_reconnect_target(0, 0, "Auto");
        assert_eq!(baud, 115200);
        assert_eq!(framing, "8N1");

        // Case 2: Last known baud
        let (baud, framing) = ConnectionManager::calculate_reconnect_target(0, 9600, "Auto");
        assert_eq!(baud, 9600);
        assert_eq!(framing, "8N1");

        // Case 3: User pref takes precedence
        let (baud, _framing) = ConnectionManager::calculate_reconnect_target(57600, 9600, "Auto");
        assert_eq!(baud, 57600);

        // Case 4: Explicit framing
        let (_baud, framing) = ConnectionManager::calculate_reconnect_target(0, 0, "7E1");
        assert_eq!(framing, "7E1");
    }
}
