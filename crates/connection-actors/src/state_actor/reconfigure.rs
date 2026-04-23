use actor_protocol::{ActorError, ConnectionState};
#[cfg(target_arch = "wasm32")]
use actor_runtime::actor_debug;
use actor_runtime::{PortMessage, ProbeMessage};
#[cfg(target_arch = "wasm32")]
use wasm_bindgen::JsCast;

impl super::StateActor {
    #[cfg_attr(not(target_arch = "wasm32"), allow(unused_variables))]
    #[cfg(target_arch = "wasm32")]
    pub(super) async fn handle_reconfigure_with_port(
        &mut self,
        baud: u32,
        framing: String,
        active_framing: String,
        port_handle: Option<actor_runtime::channels::PortHandle>,
    ) -> Result<(), ActorError> {
        if self.state != ConnectionState::Connected {
            return Err(ActorError::UnexpectedMessage {
                state: format!("{:?}", self.state),
                message: "Reconfigure".into(),
            });
        }

        // Store reconfigure parameters for event-driven completion
        // (will be processed when ConnectionClosed is received)
        self.pending_reconfigure_baud = Some(baud);
        self.pending_reconfigure_framing = Some(framing);
        self.pending_reconfigure_active_framing = Some(active_framing);

        // Use provided port_handle or fall back to stored one
        let handle = port_handle.or_else(|| self.pending_port_handle.clone());
        self.pending_port_handle = handle;

        // Transition to Disconnecting
        self.transition(ConnectionState::Disconnecting)?;

        // Tell PortActor to close (CRITICAL - must succeed for event-driven coordination)
        // Event-driven coordination: StateActor will receive ConnectionClosed
        // message from PortActor when close is complete, then proceed with reconnection
        self.send_critical_port(PortMessage::Close)?;

        Ok(())
    }

    /// Complete reconfiguration after port close (called from ConnectionClosed handler)
    #[cfg(target_arch = "wasm32")]
    pub(super) async fn complete_reconfigure(&mut self) -> Result<(), ActorError> {
        // Extract stored parameters
        let baud = self
            .pending_reconfigure_baud
            .take()
            .ok_or_else(|| ActorError::Other("Missing reconfigure baud parameter".into()))?;
        let framing = self
            .pending_reconfigure_framing
            .take()
            .ok_or_else(|| ActorError::Other("Missing reconfigure framing parameter".into()))?;
        let active_framing = self
            .pending_reconfigure_active_framing
            .take()
            .unwrap_or_else(|| "8N1".into());

        // Normalize framing:
        // - If user selected "Auto", use previously detected active_framing
        // - Otherwise use user's explicit selection
        let actual_framing = if framing.eq_ignore_ascii_case("auto") {
            active_framing
        } else {
            framing
        };

        if let Some(port) = self.pending_port_handle.clone() {
            // Extract port info from SerialPort object
            let info = if let Ok(func_val) = js_sys::Reflect::get(&port, &"getInfo".into()) {
                if let Ok(func) = func_val.dyn_into::<js_sys::Function>() {
                    func.call0(&port)
                        .unwrap_or(wasm_bindgen::JsValue::from(js_sys::Object::new()))
                } else {
                    js_sys::Object::new().into()
                }
            } else {
                js_sys::Object::new().into()
            };

            let vid = js_sys::Reflect::get(&info, &"usbVendorId".into())
                .ok()
                .and_then(|v| v.as_f64())
                .map(|v| v as u16);
            let pid = js_sys::Reflect::get(&info, &"usbProductId".into())
                .ok()
                .and_then(|v| v.as_f64())
                .map(|v| v as u16);

            let port_info = actor_protocol::SerialPortInfo {
                path: format!("{:04X}:{:04X}", vid.unwrap_or(0), pid.unwrap_or(0)),
                vid,
                pid,
            };

            if baud == 0 {
                // Auto-detect requested - transition to Probing
                actor_debug!("StateActor: Reconfigure with Auto (baud=0) - starting probing");

                self.pending_port = Some(port_info.clone());
                self.pending_baud = 0;
                self.pending_port_handle = Some(port.clone());
                self.transition(ConnectionState::Probing)?;
                self.send_critical_probe(ProbeMessage::Start {
                    port: port_info,
                    port_handle: port,
                })?;
            } else {
                // Direct connection with specified baud
                actor_debug!("StateActor: Reconfigure to {}@{}", actual_framing, baud);

                self.pending_port = Some(port_info.clone());
                self.pending_baud = baud;
                self.pending_port_handle = Some(port.clone());
                self.transition(ConnectionState::Connecting)?;
                let operation_id = self.next_operation_id();
                self.send_critical_port(PortMessage::Open {
                    port: port_info,
                    baud,
                    framing: actual_framing,
                    send_wakeup: false, // Reconfigure is manual, don't send wakeup
                    operation_id,
                    port_handle: port,
                })?;
            }
        }

        Ok(())
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub(super) async fn handle_reconfigure(
        &mut self,
        baud: u32,
        framing: String,
        active_framing: String,
    ) -> Result<(), ActorError> {
        if self.state != ConnectionState::Connected {
            return Err(ActorError::UnexpectedMessage {
                state: format!("{:?}", self.state),
                message: "Reconfigure".into(),
            });
        }

        // Transition to Disconnecting
        self.transition(ConnectionState::Disconnecting)?;

        // Tell PortActor to close (CRITICAL)
        self.send_critical_port(PortMessage::Close)?;

        // Transition to Disconnected
        self.transition(ConnectionState::Disconnected)?;

        // Normalize framing:
        // - If user selected "Auto", use previously detected active_framing
        // - Otherwise use user's explicit selection
        let actual_framing = if framing.eq_ignore_ascii_case("auto") {
            active_framing
        } else {
            framing
        };

        // Native implementation (without WASM-specific port handling)
        if let Some(port_info) = self.pending_port.clone() {
            if baud == 0 {
                self.pending_port = Some(port_info.clone());
                self.pending_baud = 0;
                self.transition(ConnectionState::Probing)?;
                self.send_critical_probe(ProbeMessage::Start { port: port_info })?;
            } else {
                self.pending_port = Some(port_info.clone());
                self.pending_baud = baud;
                self.transition(ConnectionState::Connecting)?;
                let operation_id = self.next_operation_id();
                self.send_critical_port(PortMessage::Open {
                    port: port_info,
                    baud,
                    framing: actual_framing,
                    send_wakeup: false,
                    operation_id,
                })?;
            }
        }

        Ok(())
    }
}
