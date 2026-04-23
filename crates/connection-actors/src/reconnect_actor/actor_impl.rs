use actor_protocol::ActorError;
use actor_runtime::{Actor, ReconnectMessage};

#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;

use super::{DeviceIdentity, ReconnectActor};

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
