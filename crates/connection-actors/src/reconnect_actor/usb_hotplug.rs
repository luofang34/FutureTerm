#[cfg(target_arch = "wasm32")]
use wasm_bindgen::closure::Closure;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;

#[cfg(target_arch = "wasm32")]
use std::cell::RefCell;
#[cfg(target_arch = "wasm32")]
use std::rc::Rc;

#[cfg(target_arch = "wasm32")]
use actor_runtime::StateMessage;
#[cfg(target_arch = "wasm32")]
use futures_channel::mpsc;

#[cfg(target_arch = "wasm32")]
use super::{DeviceIdentity, DeviceState, EventClosures};

impl super::ReconnectActor {
    /// Poll for device with retry logic (USB reconnection helper)
    ///
    /// This async function implements the retry logic for USB device enumeration
    /// after a hotplug event. It polls navigator.serial.getPorts() with exponential
    /// backoff until the target device is found or max retries/timeout is reached.
    #[cfg(target_arch = "wasm32")]
    async fn poll_for_device_with_retry(
        target_vid: u16,
        target_pid: u16,
        mut state_tx: mpsc::Sender<StateMessage>,
        last_device_shared: Rc<RefCell<Option<DeviceState>>>,
    ) {
        use crate::constants::reconnect::{GLOBAL_TIMEOUT_MS, INITIAL_DELAY_MS, MAX_RETRIES};

        let start_time = js_sys::Date::now();

        // Wait for initial USB enumeration
        gloo_timers::future::sleep(std::time::Duration::from_millis(INITIAL_DELAY_MS)).await;

        // Retry loop with exponential backoff
        for attempt in 1..=MAX_RETRIES {
            // Check global timeout
            let elapsed_ms = (js_sys::Date::now() - start_time) as u64;
            if elapsed_ms >= GLOBAL_TIMEOUT_MS {
                #[cfg(debug_assertions)]
                web_sys::console::log_1(
                    &"USB reconnect: Global timeout exceeded (5s). Giving up.".into(),
                );
                return; // Stay in DeviceLost state
            }

            // Check if user manually disconnected (device registration cleared)
            if last_device_shared.borrow().is_none() {
                #[cfg(debug_assertions)]
                web_sys::console::log_1(
                    &"USB reconnect aborted - device registration cleared by user".into(),
                );
                return; // Exit retry loop
            }

            #[cfg(debug_assertions)]
            {
                if attempt > 1 {
                    web_sys::console::log_1(
                        &format!(
                            "USB reconnect: Retry attempt {} of {}",
                            attempt, MAX_RETRIES
                        )
                        .into(),
                    );
                }
            }

            // Show "waiting" message on attempt 2
            #[cfg(debug_assertions)]
            {
                if attempt == 2 {
                    web_sys::console::log_1(
                        &format!(
                            "Still waiting for device {:04X}:{:04X}...",
                            target_vid, target_pid
                        )
                        .into(),
                    );
                }
            }

            // Get current port list
            let Some(window) = web_sys::window() else {
                return;
            };
            let nav = window.navigator();
            let Ok(serial) = js_sys::Reflect::get(&nav, &"serial".into()) else {
                return;
            };
            let Ok(get_ports_fn) = js_sys::Reflect::get(&serial, &"getPorts".into()) else {
                return;
            };
            let Ok(get_ports) = get_ports_fn.dyn_into::<js_sys::Function>() else {
                return;
            };
            let Ok(promise) = get_ports.call0(&serial) else {
                return;
            };
            let Ok(promise_obj) = promise.dyn_into::<js_sys::Promise>() else {
                return;
            };

            // Await ports promise
            let Ok(ports_value) = wasm_bindgen_futures::JsFuture::from(promise_obj).await else {
                return;
            };
            let Ok(ports) = ports_value.dyn_into::<js_sys::Array>() else {
                return;
            };

            // Check each port for matching VID/PID
            for i in 0..ports.length() {
                let Some(port_value) = ports.get(i).dyn_into::<web_sys::SerialPort>().ok() else {
                    continue;
                };

                // Get port info
                let Ok(get_info_fn) = js_sys::Reflect::get(&port_value, &"getInfo".into()) else {
                    continue;
                };
                let Ok(func) = get_info_fn.dyn_into::<js_sys::Function>() else {
                    continue;
                };
                let Ok(info) = func.call0(&port_value) else {
                    continue;
                };

                // Extract VID/PID
                let vid = js_sys::Reflect::get(&info, &"usbVendorId".into())
                    .ok()
                    .and_then(|v| v.as_f64())
                    .map(|v| v as u16);
                let pid = js_sys::Reflect::get(&info, &"usbProductId".into())
                    .ok()
                    .and_then(|v| v.as_f64())
                    .map(|v| v as u16);

                // Check if this matches our target device
                if vid == Some(target_vid) && pid == Some(target_pid) {
                    #[cfg(debug_assertions)]
                    web_sys::console::log_1(
                        &format!(
                            "Matched device: {:04X}:{:04X} on attempt {}",
                            target_vid, target_pid, attempt
                        )
                        .into(),
                    );

                    let port_info = actor_protocol::SerialPortInfo {
                        path: format!("{:04X}:{:04X}", target_vid, target_pid),
                        vid,
                        pid,
                    };

                    // Send DeviceReappeared message to StateActor with port handle
                    let port_handle = std::rc::Rc::new(port_value);
                    let _ = state_tx.try_send(StateMessage::DeviceReappeared {
                        port: port_info,
                        port_handle,
                    });

                    return; // Success! Exit retry loop
                }
            }

            // Device not found in this attempt
            if attempt < MAX_RETRIES {
                // Calculate backoff delay using shared module
                let delay_ms = crate::backoff::calculate_retry_delay(attempt);

                #[cfg(debug_assertions)]
                web_sys::console::log_1(
                    &format!(
                        "Device {:04X}:{:04X} not found, waiting {}ms before retry",
                        target_vid, target_pid, delay_ms
                    )
                    .into(),
                );

                gloo_timers::future::sleep(std::time::Duration::from_millis(delay_ms)).await;
            } else {
                // Final attempt failed - stay in DeviceLost, USB handler keeps listening
                #[cfg(debug_assertions)]
                web_sys::console::log_1(
                    &format!(
                        "Device {:04X}:{:04X} not found after {} attempts (~{:.1}s). Staying in DeviceLost - USB event handler will detect when device reappears. Click Disconnect to abort.",
                        target_vid, target_pid, MAX_RETRIES,
                        (js_sys::Date::now() - start_time) / 1000.0
                    )
                    .into(),
                );
            }
        }
    }

    pub(super) fn setup_event_listeners(&mut self) {
        #[cfg(target_arch = "wasm32")]
        {
            let Some(window) = web_sys::window() else {
                return;
            };
            let navigator = window.navigator();
            let Ok(serial) = js_sys::Reflect::get(&navigator, &"serial".into()) else {
                #[cfg(debug_assertions)]
                web_sys::console::warn_1(&"navigator.serial not available".into());
                return;
            };

            // Clone state_tx for closures
            let state_tx_for_connect = self.state_tx.clone();
            let state_tx_for_disconnect = self.state_tx.clone();

            // Clone shared state for the connect handler to check
            let last_device_shared = self.last_device_shared.clone();

            // Create onconnect closure
            // When a USB device connects, we need to poll all ports and check if it matches our registered device
            let onconnect =
                Closure::<dyn FnMut(web_sys::Event)>::new(move |_event: web_sys::Event| {
                    #[cfg(debug_assertions)]
                    web_sys::console::log_1(&"USB connect event received".into());

                    // Check if we have a registered device to reconnect to
                    let target_device: DeviceIdentity = match last_device_shared.borrow().as_ref() {
                        Some(dev_state) => dev_state.identity.clone(),
                        None => {
                            #[cfg(debug_assertions)]
                            web_sys::console::log_1(
                                &"No device registered for auto-reconnect".into(),
                            );
                            return;
                        }
                    };

                    // Clone state_tx for async task
                    let state_tx = state_tx_for_connect.clone();
                    let target_vid = target_device.vid;
                    let target_pid = target_device.pid;

                    // Clone shared state for checking if user manually disconnected
                    let last_device_for_retry = last_device_shared.clone();

                    // Spawn async task with retry logic (extracted to reduce closure complexity)
                    wasm_bindgen_futures::spawn_local(async move {
                        Self::poll_for_device_with_retry(
                            target_vid,
                            target_pid,
                            state_tx,
                            last_device_for_retry,
                        )
                        .await;
                    });
                });

            // Create ondisconnect closure
            // Clone shared state to check if we have a registered device
            let last_device_for_disconnect = self.last_device_shared.clone();

            let ondisconnect =
                Closure::<dyn FnMut(web_sys::Event)>::new(move |_event: web_sys::Event| {
                    #[cfg(debug_assertions)]
                    web_sys::console::log_1(&"USB disconnect event received".into());

                    // Only send ConnectionLost if device was actually connected
                    // This prevents false positives from unrelated USB devices
                    if let Some(device_state) = last_device_for_disconnect.borrow().as_ref() {
                        if device_state.is_connected {
                            let _ = state_tx_for_disconnect
                                .clone()
                                .try_send(StateMessage::ConnectionLost);
                        } else {
                            #[cfg(debug_assertions)]
                            web_sys::console::log_1(
                                &"USB disconnect ignored - device not connected".into(),
                            );
                        }
                    } else {
                        #[cfg(debug_assertions)]
                        web_sys::console::log_1(
                            &"USB disconnect ignored - no device registered".into(),
                        );
                    }
                });

            // Register event listeners
            if let Ok(serial_obj) = serial.dyn_into::<web_sys::EventTarget>() {
                let _ = serial_obj.add_event_listener_with_callback(
                    "connect",
                    onconnect.as_ref().unchecked_ref(),
                );
                let _ = serial_obj.add_event_listener_with_callback(
                    "disconnect",
                    ondisconnect.as_ref().unchecked_ref(),
                );

                // Store closures to prevent drop
                self.event_closures = Some(EventClosures {
                    _onconnect: onconnect,
                    _ondisconnect: ondisconnect,
                });

                #[cfg(debug_assertions)]
                web_sys::console::log_1(&"USB event listeners registered successfully".into());
            }
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            #[cfg(debug_assertions)]
            eprintln!("USB event listeners not available on native platform");
        }
    }
}
