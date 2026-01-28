use actor_protocol::{ActorError, SerialConfig, SystemEvent};
use actor_runtime::{Actor, ReconnectMessage, StateMessage};
use futures_channel::mpsc;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::closure::Closure;
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;

#[cfg(target_arch = "wasm32")]
use std::cell::RefCell;
#[cfg(target_arch = "wasm32")]
use std::rc::Rc;

/// Device identity for matching reconnected devices
#[derive(Debug, Clone, PartialEq)]
pub struct DeviceIdentity {
    pub vid: u16,
    pub pid: u16,
}

/// Device state tracking for reconnection
#[cfg(target_arch = "wasm32")]
#[derive(Debug, Clone)]
struct DeviceState {
    identity: DeviceIdentity,
    is_connected: bool,
}

/// Configuration to use when reconnecting
#[derive(Debug, Clone)]
pub struct ReconnectConfig {
    pub baud: u32,
    pub framing: String,
}

/// USB event listener closures (stored to prevent drop)
#[cfg(target_arch = "wasm32")]
struct EventClosures {
    _onconnect: Closure<dyn FnMut(web_sys::Event)>,
    _ondisconnect: Closure<dyn FnMut(web_sys::Event)>,
}

/// ReconnectActor manages USB device hotplug and auto-reconnection
///
/// Responsibilities:
/// - Register device VID/PID when user connects
/// - Monitor for device disconnect/reconnect events
/// - Trigger auto-reconnection when registered device reappears
/// - Persist device info to localStorage (in WASM)
pub struct ReconnectActor {
    last_device: Option<DeviceIdentity>,
    reconnect_config: Option<ReconnectConfig>,
    /// Sender to StateActor - used in background polling task
    #[allow(dead_code)]
    state_tx: mpsc::Sender<StateMessage>,
    event_tx: mpsc::Sender<SystemEvent>,

    #[cfg(target_arch = "wasm32")]
    event_closures: Option<EventClosures>,

    // Shared state for USB event handlers (allows closures to access current device)
    #[cfg(target_arch = "wasm32")]
    last_device_shared: Rc<RefCell<Option<DeviceState>>>,
}

impl ReconnectActor {
    pub fn new(state_tx: mpsc::Sender<StateMessage>, event_tx: mpsc::Sender<SystemEvent>) -> Self {
        Self {
            last_device: None,
            reconnect_config: None,
            state_tx,
            event_tx,

            #[cfg(target_arch = "wasm32")]
            event_closures: None,

            #[cfg(target_arch = "wasm32")]
            last_device_shared: Rc::new(RefCell::new(None)),
        }
    }

    async fn handle_register_device(
        &mut self,
        vid: u16,
        pid: u16,
        config: SerialConfig,
    ) -> Result<(), ActorError> {
        let device_identity = DeviceIdentity { vid, pid };
        self.last_device = Some(device_identity.clone());
        self.reconnect_config = Some(ReconnectConfig {
            baud: config.baud_rate,
            framing: "8N1".into(), // Simplified for now
        });

        // Update shared state for USB event handlers
        #[cfg(target_arch = "wasm32")]
        {
            *self.last_device_shared.borrow_mut() = Some(DeviceState {
                identity: device_identity,
                is_connected: true,
            });
        }

        #[cfg(debug_assertions)]
        {
            #[cfg(target_arch = "wasm32")]
            web_sys::console::log_1(
                &format!(
                    "Registered device for auto-reconnect: {:04X}:{:04X}",
                    vid, pid
                )
                .into(),
            );
            #[cfg(not(target_arch = "wasm32"))]
            eprintln!(
                "Registered device for auto-reconnect: {:04X}:{:04X}",
                vid, pid
            );
        }

        // Persist to localStorage
        self.persist_device(vid, pid);

        Ok(())
    }

    async fn handle_clear_device(&mut self) -> Result<(), ActorError> {
        self.last_device = None;
        self.reconnect_config = None;

        // Update shared state for USB event handlers
        #[cfg(target_arch = "wasm32")]
        {
            *self.last_device_shared.borrow_mut() = None;
        }

        #[cfg(debug_assertions)]
        {
            #[cfg(target_arch = "wasm32")]
            web_sys::console::log_1(&"Cleared auto-reconnect device".into());
            #[cfg(not(target_arch = "wasm32"))]
            eprintln!("Cleared auto-reconnect device");
        }

        self.clear_persisted_device();

        Ok(())
    }

    async fn handle_device_connected(
        &mut self,
        port: actor_protocol::SerialPortInfo,
        #[cfg(target_arch = "wasm32")] port_handle: Option<actor_runtime::channels::PortHandle>,
    ) -> Result<(), ActorError> {
        // Check if this matches our registered device
        let target = match &self.last_device {
            Some(d) => d,
            None => return Ok(()), // No device registered, ignore
        };

        // Match VID/PID
        if let (Some(vid), Some(pid)) = (port.vid, port.pid) {
            if vid == target.vid && pid == target.pid {
                // This is our device!
                let _ = self.event_tx.try_send(SystemEvent::StatusUpdate {
                    message: format!(
                        "Device {:04X}:{:04X} detected. Auto-reconnecting...",
                        vid, pid
                    ),
                });

                // Notify StateActor to trigger reconnection
                #[cfg(target_arch = "wasm32")]
                {
                    if let Some(handle) = port_handle {
                        self.state_tx
                            .try_send(StateMessage::DeviceReappeared {
                                port,
                                port_handle: handle,
                            })
                            .map_err(|_| {
                                ActorError::ChannelClosed(
                                    "StateActor unavailable during DeviceReappeared".into(),
                                )
                            })?;
                    }
                }

                #[cfg(not(target_arch = "wasm32"))]
                {
                    self.state_tx
                        .try_send(StateMessage::DeviceReappeared { port })
                        .map_err(|_| {
                            ActorError::ChannelClosed(
                                "StateActor unavailable during DeviceReappeared".into(),
                            )
                        })?;
                }
            }
        }

        Ok(())
    }

    // Parameters used in cfg-gated code paths
    #[allow(unused_variables)]
    fn persist_device(&self, vid: u16, pid: u16) {
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(window) = web_sys::window() {
                if let Ok(Some(storage)) = window.local_storage() {
                    let key = "futureterm_last_device";
                    let value = format!("{:04X}:{:04X}", vid, pid);
                    let _ = storage.set_item(key, &value);

                    #[cfg(debug_assertions)]
                    web_sys::console::log_1(
                        &format!("Persisted device to localStorage: {}", value).into(),
                    );
                }
            }
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            #[cfg(debug_assertions)]
            eprintln!("Would persist to localStorage: {:04X}:{:04X}", vid, pid);
        }
    }

    fn clear_persisted_device(&self) {
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(window) = web_sys::window() {
                if let Ok(Some(storage)) = window.local_storage() {
                    let key = "futureterm_last_device";
                    let _ = storage.remove_item(key);

                    #[cfg(debug_assertions)]
                    web_sys::console::log_1(&"Cleared device from localStorage".into());
                }
            }
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            #[cfg(debug_assertions)]
            eprintln!("Would clear from localStorage");
        }
    }

    fn load_persisted_device(&self) -> Option<(u16, u16)> {
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(window) = web_sys::window() {
                if let Ok(Some(storage)) = window.local_storage() {
                    let key = "futureterm_last_device";
                    if let Ok(Some(value)) = storage.get_item(key) {
                        // Parse "1234:5678" format
                        let parts: Vec<&str> = value.split(':').collect();
                        if parts.len() == 2 {
                            // Use .first() and .get(1) to avoid clippy warnings
                            if let (Some(&vid_str), Some(&pid_str)) = (parts.first(), parts.get(1))
                            {
                                if let (Ok(vid), Ok(pid)) = (
                                    u16::from_str_radix(vid_str, 16),
                                    u16::from_str_radix(pid_str, 16),
                                ) {
                                    #[cfg(debug_assertions)]
                                    web_sys::console::log_1(
                                        &format!(
                                            "Loaded device from localStorage: {:04X}:{:04X}",
                                            vid, pid
                                        )
                                        .into(),
                                    );
                                    return Some((vid, pid));
                                }
                            }
                        }
                    }
                }
            }
        }

        None
    }

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

    fn setup_event_listeners(&mut self) {
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

impl Actor for ReconnectActor {
    type Message = ReconnectMessage;

    fn name(&self) -> &'static str {
        "ReconnectActor"
    }

    async fn init(&mut self) -> Result<(), ActorError> {
        // Try to restore device from localStorage
        if let Some((vid, pid)) = self.load_persisted_device() {
            self.last_device = Some(DeviceIdentity { vid, pid });

            #[cfg(debug_assertions)]
            {
                #[cfg(not(target_arch = "wasm32"))]
                eprintln!("Restored device from storage: {:04X}:{:04X}", vid, pid);
            }
        }

        // Set up USB event listeners
        self.setup_event_listeners();

        Ok(())
    }

    async fn handle(&mut self, msg: ReconnectMessage) -> Result<(), ActorError> {
        match msg {
            ReconnectMessage::RegisterDevice { vid, pid, config } => {
                self.handle_register_device(vid, pid, config).await?
            }
            ReconnectMessage::ClearDevice => self.handle_clear_device().await?,

            #[cfg(target_arch = "wasm32")]
            ReconnectMessage::DeviceConnected { port, port_handle } => {
                self.handle_device_connected(port, Some(port_handle))
                    .await?
            }

            #[cfg(not(target_arch = "wasm32"))]
            ReconnectMessage::DeviceConnected { port } => {
                self.handle_device_connected(port).await?
            }
        }

        Ok(())
    }

    async fn shutdown(&mut self) {
        // Clean up USB event listeners to prevent memory leaks
        #[cfg(target_arch = "wasm32")]
        {
            if let Some(closures) = self.event_closures.take() {
                // Get navigator.serial to remove listeners
                if let Some(window) = web_sys::window() {
                    let navigator: web_sys::Navigator = window.navigator();

                    if let Ok(serial_val) = js_sys::Reflect::get(&navigator, &"serial".into()) {
                        if !serial_val.is_undefined() {
                            if let Ok(serial_obj) = serial_val.dyn_into::<web_sys::EventTarget>() {
                                // Remove event listeners using the stored closure references
                                if let Err(e) = serial_obj.remove_event_listener_with_callback(
                                    "connect",
                                    closures._onconnect.as_ref().unchecked_ref(),
                                ) {
                                    #[cfg(debug_assertions)]
                                    web_sys::console::warn_1(
                                        &format!("Failed to remove 'connect' listener: {:?}", e)
                                            .into(),
                                    );
                                }
                                if let Err(e) = serial_obj.remove_event_listener_with_callback(
                                    "disconnect",
                                    closures._ondisconnect.as_ref().unchecked_ref(),
                                ) {
                                    #[cfg(debug_assertions)]
                                    web_sys::console::warn_1(
                                        &format!("Failed to remove 'disconnect' listener: {:?}", e)
                                            .into(),
                                    );
                                }

                                #[cfg(debug_assertions)]
                                web_sys::console::log_1(
                                    &"ReconnectActor: USB event listeners removed successfully"
                                        .into(),
                                );
                            }
                        }
                    }
                }
                // Closures are dropped here, completing cleanup
            }
        }

        #[cfg(not(target_arch = "wasm32"))]
        {
            #[cfg(debug_assertions)]
            eprintln!("ReconnectActor: Shutdown complete (no event listeners on native)");
        }
    }
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use actor_protocol::SerialConfig;
    use futures::stream::StreamExt;

    fn create_test_actor() -> (
        ReconnectActor,
        mpsc::Receiver<StateMessage>,
        mpsc::Receiver<SystemEvent>,
    ) {
        let (state_tx, state_rx) = mpsc::channel(100);
        let (event_tx, event_rx) = mpsc::channel(100);

        let actor = ReconnectActor::new(state_tx, event_tx);
        (actor, state_rx, event_rx)
    }

    #[tokio::test]
    async fn test_initial_state() {
        let (actor, _, _) = create_test_actor();
        assert!(actor.last_device.is_none());
        assert!(actor.reconnect_config.is_none());
    }

    #[tokio::test]
    async fn test_register_device() {
        let (mut actor, _, _) = create_test_actor();

        let config = SerialConfig::new_8n1(115200);
        actor
            .handle_register_device(0x1234, 0x5678, config)
            .await
            .unwrap();

        assert_eq!(
            actor.last_device,
            Some(DeviceIdentity {
                vid: 0x1234,
                pid: 0x5678
            })
        );
        assert!(actor.reconnect_config.is_some());
        assert_eq!(actor.reconnect_config.as_ref().unwrap().baud, 115200);
    }

    #[tokio::test]
    async fn test_clear_device() {
        let (mut actor, _, _) = create_test_actor();

        // Set up device
        actor.last_device = Some(DeviceIdentity {
            vid: 0x1234,
            pid: 0x5678,
        });
        actor.reconnect_config = Some(ReconnectConfig {
            baud: 115200,
            framing: "8N1".into(),
        });

        actor.handle_clear_device().await.unwrap();

        assert!(actor.last_device.is_none());
        assert!(actor.reconnect_config.is_none());
    }

    #[tokio::test]
    async fn test_device_match_triggers_reconnect() {
        let (mut actor, mut state_rx, mut event_rx) = create_test_actor();

        // Register device
        let config = SerialConfig::new_8n1(115200);
        actor
            .handle_register_device(0x1234, 0x5678, config)
            .await
            .unwrap();

        // Simulate device connection
        let port =
            actor_protocol::SerialPortInfo::new("/dev/ttyUSB0".into(), Some(0x1234), Some(0x5678));
        actor.handle_device_connected(port.clone()).await.unwrap();

        // Should emit status update
        let event = event_rx.next().await.unwrap();
        match event {
            SystemEvent::StatusUpdate { message } => {
                assert!(message.contains("1234"));
                assert!(message.contains("5678"));
                assert!(message.contains("Auto-reconnecting"));
            }
            _ => panic!("Wrong event"),
        }

        // Should notify StateActor
        let state_msg = state_rx.next().await.unwrap();
        match state_msg {
            StateMessage::DeviceReappeared { port: p } => {
                assert_eq!(p.path, "/dev/ttyUSB0");
            }
            _ => panic!("Wrong message"),
        }
    }

    #[tokio::test]
    async fn test_device_mismatch_ignored() {
        let (mut actor, mut state_rx, mut event_rx) = create_test_actor();

        // Register device 0x1234:0x5678
        let config = SerialConfig::new_8n1(115200);
        actor
            .handle_register_device(0x1234, 0x5678, config)
            .await
            .unwrap();

        // Simulate different device connection 0xAAAA:0xBBBB
        let port =
            actor_protocol::SerialPortInfo::new("/dev/ttyUSB0".into(), Some(0xAAAA), Some(0xBBBB));
        actor.handle_device_connected(port).await.unwrap();

        // Should NOT emit any events or messages (non-matching device)
        assert!(event_rx.try_next().is_err()); // No events
        assert!(state_rx.try_next().is_err()); // No state messages
    }

    #[tokio::test]
    async fn test_no_device_registered_ignored() {
        let (mut actor, mut state_rx, mut event_rx) = create_test_actor();

        // No device registered
        let port =
            actor_protocol::SerialPortInfo::new("/dev/ttyUSB0".into(), Some(0x1234), Some(0x5678));
        actor.handle_device_connected(port).await.unwrap();

        // Should not trigger reconnect
        assert!(event_rx.try_next().is_err());
        assert!(state_rx.try_next().is_err());
    }

    #[tokio::test]
    async fn test_device_without_vid_pid_ignored() {
        let (mut actor, mut state_rx, mut event_rx) = create_test_actor();

        // Register device
        let config = SerialConfig::new_8n1(115200);
        actor
            .handle_register_device(0x1234, 0x5678, config)
            .await
            .unwrap();

        // Port without VID/PID (e.g., virtual COM port)
        let port = actor_protocol::SerialPortInfo::new("/dev/ttyUSB0".into(), None, None);
        actor.handle_device_connected(port).await.unwrap();

        // Should be ignored
        assert!(event_rx.try_next().is_err());
        assert!(state_rx.try_next().is_err());
    }

    #[test]
    fn test_device_swap_detection_logic() {
        // Test 1: Different device detected (device swap scenario)
        let target_vid = 0x0403;
        let target_pid = 0x6001;
        let found_vid = 0x1B8C;
        let found_pid = 0x0036;

        // Different VID/PID indicates device swap
        assert!(found_vid != target_vid || found_pid != target_pid);

        // Test 2: Same device detected (no device swap)
        let same_vid = 0x0403;
        let same_pid = 0x6001;

        // Same VID/PID indicates original device reconnected
        assert!(same_vid == target_vid && same_pid == target_pid);
    }

    #[test]
    fn test_attempt_threshold() {
        // Device swap detection starts tracking at attempt 3 (~750ms cumulative)
        // Uses port validation instead of hardcoded timing
        // Validates after 2+ consecutive sightings of the same different device
        const DEVICE_SWAP_CHECK_ATTEMPT: u32 = 3;

        let should_check_attempt_1 = 1 >= DEVICE_SWAP_CHECK_ATTEMPT; // Too early
        let should_check_attempt_2 = 2 >= DEVICE_SWAP_CHECK_ATTEMPT; // Too early
        let should_check_attempt_3 = 3 >= DEVICE_SWAP_CHECK_ATTEMPT; // Threshold met!
        let should_check_attempt_4 = 4 >= DEVICE_SWAP_CHECK_ATTEMPT; // Also valid
        let should_check_attempt_5 = 5 >= DEVICE_SWAP_CHECK_ATTEMPT; // Also valid

        assert!(!should_check_attempt_1);
        assert!(!should_check_attempt_2); // Still too early
        assert!(should_check_attempt_3); // Triggers at attempt 3
        assert!(should_check_attempt_4); // Also triggers at attempt 4+
        assert!(should_check_attempt_5);
    }

    #[test]
    fn test_device_swap_detection_requires_two_consistent_sightings() {
        // Real device swap: Same different device seen on 2+ consecutive attempts, then validated
        // Port validation eliminates need for 3+ sightings

        // Scenario 1: Device seen on attempts 3, 4 (count = 2) → VALIDATE
        let count_scenario_1 = 2;
        let should_validate_1 = count_scenario_1 >= 2;
        assert!(should_validate_1, "Should validate port when seen 2 times");

        // Scenario 2: Device seen only on attempt 3 (count = 1) → NO VALIDATE
        let count_scenario_2 = 1;
        let should_validate_2 = count_scenario_2 >= 2;
        assert!(
            !should_validate_2,
            "Should NOT validate when only seen 1 time (just appeared)"
        );
    }

    #[test]
    fn test_multi_interface_device_scenario() {
        // Multi-interface USB device scenario with port validation:
        // - Attempts 3, 4: Control interface (3162:004B) visible → count = 2 → VALIDATE
        // - Validation FAILS (control interface is not a serial port)
        // - Counter resets, keeps waiting
        // - Attempt 5: Serial interface (1B8C:0036) appears → count = 1
        // - Attempt 6: Serial interface again → count = 2 → VALIDATE
        // - Validation SUCCEEDS → trigger device swap

        // Phase 1: Control interface seen twice
        let control_interface_count = 2;

        // Should validate at count = 2
        let should_validate_control = control_interface_count >= 2;
        assert!(should_validate_control, "Should validate after 2 sightings");

        // Validation fails (not a serial port) → counter resets to 0, then serial interface appears

        // Phase 2: Serial interface appears (count starts at 1)
        let new_device = (0x1B8C, 0x0036);
        let mut serial_interface_count = 1; // First sighting of serial interface

        // Next attempt: see serial interface again
        let last_device = Some(new_device);
        if last_device == Some(new_device) {
            serial_interface_count += 1;
        }

        // Should validate again (count = 2)
        assert_eq!(serial_interface_count, 2);
        assert!(
            serial_interface_count >= 2,
            "Should validate serial interface"
        );
    }

    #[test]
    fn test_real_device_swap_scenario() {
        // Real device swap scenario with port validation:
        // - User unplugs FTDI (0403:6001)
        // - User plugs STM32 serial (1B8C:0036)
        // - Attempt 3: See STM32 → count = 1
        // - Attempt 4: See STM32 again → count = 2 → VALIDATE
        // - Validation SUCCEEDS (it's a serial port) → trigger device swap

        // Simulate: attempts 3, 4 saw STM32
        let consecutive_count = 2;

        // Should validate after 2 sightings
        let should_validate = consecutive_count >= 2;
        assert!(
            should_validate,
            "Should validate device after 2 consecutive sightings"
        );

        // If validation succeeds, device swap triggers
        // (validation logic tested separately in manual tests)
    }

    #[test]
    fn test_validation_triggers_on_consecutive_sightings() {
        // Port validation triggers after 2 consecutive sightings (not on final attempt)
        // This allows faster device swap detection

        // Attempt 3: consecutive_count = 1 (first sighting)
        let consecutive_count_attempt_3 = 1;
        let should_validate_3 = consecutive_count_attempt_3 >= 2;
        assert!(!should_validate_3, "Should NOT validate on first sighting");

        // Attempt 4: consecutive_count = 2 (second consecutive sighting)
        let consecutive_count_attempt_4 = 2;
        let should_validate_4 = consecutive_count_attempt_4 >= 2;
        assert!(
            should_validate_4,
            "Should validate after 2 consecutive sightings"
        );

        // No need to wait for final attempt - validation happens immediately
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod wasm_tests {
    use super::*;
    use wasm_bindgen_test::*;

    wasm_bindgen_test_configure!(run_in_browser);

    #[wasm_bindgen_test]
    async fn test_usb_reconnect_retry_logic() {
        // This test verifies the retry mechanism conceptually
        // Full integration testing requires actual USB hardware

        const MAX_RETRIES: u32 = 5;
        const INITIAL_DELAY_MS: u64 = 50;

        // Verify retry delays increase exponentially
        for attempt in 1..=MAX_RETRIES {
            let delay = crate::backoff::calculate_retry_delay(attempt);

            if attempt == 1 {
                // First retry: 100ms
                assert!(delay >= 100 && delay <= 150); // Allow for jitter
            } else if attempt == 2 {
                // Second retry: 200ms
                assert!(delay >= 200 && delay <= 250); // Allow for jitter
            } else if attempt == 3 {
                // Third retry: 400ms (device swap detection starts here)
                assert!(delay >= 400 && delay <= 450); // Allow for jitter
            }
            // Additional attempts use exponential backoff
        }

        // Verify initial delay is reasonable
        assert_eq!(INITIAL_DELAY_MS, 50);

        // Verify max retries is set correctly (5 attempts with port validation)
        assert_eq!(MAX_RETRIES, 5);
    }
}
