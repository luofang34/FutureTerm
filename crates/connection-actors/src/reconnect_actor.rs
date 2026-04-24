use actor_protocol::{ActorError, SerialConfig, SystemEvent};
use actor_runtime::StateMessage;
use futures_channel::mpsc;

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::closure::Closure;

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
}

mod actor_impl;
mod usb_hotplug;

#[cfg(test)]
mod tests;

#[cfg(all(test, target_arch = "wasm32"))]
mod wasm_tests;
