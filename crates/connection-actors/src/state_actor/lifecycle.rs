use actor_protocol::{ActorError, ConnectionState, SystemEvent};
use actor_runtime::{actor_debug, actor_info, PortMessage, ProbeMessage, ReconnectMessage};

use crate::data_processing::{detect_meaningful_content, trim_shell_artifacts};

impl super::StateActor {
    pub(super) async fn handle_connect(
        &mut self,
        port: actor_protocol::SerialPortInfo,
        baud: u32,
        framing: String,
        #[cfg(target_arch = "wasm32")] port_handle: Option<actor_runtime::channels::PortHandle>,
    ) -> Result<(), ActorError> {
        // Validate current state
        if self.state != ConnectionState::Disconnected {
            return Err(ActorError::UnexpectedMessage {
                state: format!("{:?}", self.state),
                message: "Connect".into(),
            });
        }

        // Normalize framing - if "Auto" or empty, default to "8N1" for non-probing connections
        let actual_framing = if framing.is_empty() || framing.eq_ignore_ascii_case("auto") {
            "8N1".to_string()
        } else {
            framing
        };

        // Store port handle for later use
        #[cfg(target_arch = "wasm32")]
        {
            self.pending_port_handle = port_handle.clone();
        }

        if baud == 0 {
            // Auto-detect requested - store port and go to Probing
            self.pending_port = Some(port.clone());
            self.pending_baud = 0; // Will be set by ProbeComplete
            self.transition(ConnectionState::Probing)?;

            #[cfg(target_arch = "wasm32")]
            {
                if let Some(handle) = port_handle {
                    self.send_critical_probe(ProbeMessage::Start {
                        port,
                        port_handle: handle,
                    })?;
                } else {
                    return Err(ActorError::Transport("No port handle provided".into()));
                }
            }

            #[cfg(not(target_arch = "wasm32"))]
            {
                self.send_critical_probe(ProbeMessage::Start { port })?;
            }
        } else {
            // Direct connection with specified baud (no auto-detection)
            self.pending_port = Some(port.clone());
            self.pending_baud = baud;
            self.transition(ConnectionState::Connecting)?;

            let operation_id = self.next_operation_id();

            #[cfg(target_arch = "wasm32")]
            {
                if let Some(handle) = port_handle {
                    self.send_critical_port(PortMessage::Open {
                        port,
                        baud,
                        framing: actual_framing,
                        send_wakeup: true,
                        operation_id,
                        port_handle: handle,
                    })?;
                } else {
                    return Err(ActorError::Transport("No port handle provided".into()));
                }
            }

            #[cfg(not(target_arch = "wasm32"))]
            {
                self.send_critical_port(PortMessage::Open {
                    port,
                    baud,
                    framing: actual_framing,
                    send_wakeup: true,
                    operation_id,
                })?;
            }
        }

        Ok(())
    }

    pub(super) async fn handle_disconnect(&mut self) -> Result<(), ActorError> {
        // Already disconnected - treat as no-op.
        // This can happen in bridge mode where the StateActor is never informed
        // of bridge state transitions (bridge uses set_connection_state() directly).
        if self.state == ConnectionState::Disconnected {
            return Ok(());
        }

        // Can disconnect from most states
        if !self.state.can_disconnect() {
            return Err(ActorError::UnexpectedMessage {
                state: format!("{:?}", self.state),
                message: "Disconnect".into(),
            });
        }

        // Abort any ongoing probe (CRITICAL - must succeed to cancel operation)
        if self.state == ConnectionState::Probing {
            self.send_critical_probe(ProbeMessage::Abort)?;
        }

        // Clear pending port
        self.pending_port = None;
        self.pending_baud = 0;

        // Clear device registration for auto-reconnect (non-critical hint)
        self.send_reconnect_hint(ReconnectMessage::ClearDevice);

        self.transition(ConnectionState::Disconnecting)?;

        // Tell PortActor to close (CRITICAL - must succeed for resource cleanup)
        self.send_critical_port(PortMessage::Close)?;

        // Event-driven coordination: StateActor will receive ConnectionClosed
        // message from PortActor when close is complete, then transition to Disconnected
        // (See handle() method for ConnectionClosed case)

        Ok(())
    }

    pub(super) async fn handle_connection_established(
        &mut self,
        operation_id: u32,
    ) -> Result<(), ActorError> {
        // Can happen from Connecting (manual connect) or AutoReconnecting (USB replug)
        if self.state != ConnectionState::Connecting
            && self.state != ConnectionState::AutoReconnecting
        {
            return Err(ActorError::UnexpectedMessage {
                state: format!("{:?}", self.state),
                message: "ConnectionEstablished".into(),
            });
        }

        // Validate operation ID to prevent orphan ports from timed-out operations
        if operation_id != self.operation_sequence {
            actor_debug!(
                "Ignoring stale ConnectionEstablished (operation_id={}, expected={})",
                operation_id,
                self.operation_sequence
            );

            // Close the orphan port to prevent resource leak (CRITICAL)
            self.send_critical_port(PortMessage::Close)?;

            return Err(ActorError::InvalidTransition(
                "Stale ConnectionEstablished".to_string(),
            ));
        }

        self.transition(ConnectionState::Connected)?;

        // Register device for auto-reconnect if we have VID/PID (non-critical hint)
        if let Some(ref port) = self.pending_port {
            if let (Some(vid), Some(pid)) = (port.vid, port.pid) {
                let config = actor_protocol::SerialConfig::new_8n1(self.pending_baud);

                self.send_reconnect_hint(ReconnectMessage::RegisterDevice { vid, pid, config });

                #[cfg(debug_assertions)]
                {
                    actor_info!(
                        "Registered device {:04X}:{:04X} for auto-reconnect",
                        vid,
                        pid
                    );
                }
            }
        }

        Ok(())
    }

    pub(super) async fn handle_connection_failed(
        &mut self,
        reason: String,
    ) -> Result<(), ActorError> {
        // Emit error event (non-critical UI notification)
        self.send_ui_event(SystemEvent::Error {
            message: format!("Connection failed: {}", reason),
        });

        // Return to disconnected
        self.transition(ConnectionState::Disconnected)?;

        Ok(())
    }

    pub(super) async fn handle_probe_complete(
        &mut self,
        baud: u32,
        framing: String,
        protocol: Option<String>,
        initial_data: Vec<u8>,
    ) -> Result<(), ActorError> {
        if self.state != ConnectionState::Probing {
            // Fix #9: Race condition - User disconnected during probe
            // Just ignore the message instead of erroring
            actor_debug!(
                "StateActor: Ignoring ProbeComplete in {:?} state",
                self.state
            );
            return Ok(());
        }

        // Notify UI of detection result (non-critical)
        let msg = if let Some(ref proto) = protocol {
            format!("Detected: {} @ {} baud ({})", proto, baud, framing)
        } else {
            format!("Detected: {} baud ({})", baud, framing)
        };

        self.send_ui_event(SystemEvent::StatusUpdate { message: msg });

        // Notify UI of probe results (for active_framing preservation)
        self.send_ui_event(SystemEvent::ProbeComplete {
            baud,
            framing: framing.clone(),
            protocol: protocol.clone(),
        });

        // If protocol detected (e.g., MAVLink), change decoder automatically
        if let Some(ref proto) = protocol {
            self.send_ui_event(SystemEvent::DecoderChanged { id: proto.clone() });
        }

        // Store detected baud rate for later device registration
        self.pending_baud = baud;

        // Transition to connecting
        self.transition(ConnectionState::Connecting)?;

        // CRITICAL: Open the port with detected settings
        // Clone pending_port instead of taking it, so it's still available for device registration
        if let Some(port) = self.pending_port.clone() {
            // Start with detected settings
            // Protocol-Aware Logic:
            // - If a protocol (MAVLink, etc) is detected, preserve data exactly (vital stream).
            // - If Unknown/Raw (likely Shell), we want the prompt on Line 1.
            //   The probe sent '\r', which likely echoed '\r\n' before the prompt.
            //   We then DISABLE wakeup to avoid a second prompt.
            let (send_wakeup, data_to_inject) = if protocol.is_some() {
                // Protocol detected (e.g., MAVLink) - preserve data exactly
                (false, initial_data)
            } else {
                // Unknown/Raw data (likely shell) - clean up ANSI and whitespace
                let trimmed = trim_shell_artifacts(&initial_data);

                actor_debug!(
                    "StateActor: Smart Trimmed! Skipped {} bytes. New Len: {}",
                    initial_data.len() - trimmed.len(),
                    trimmed.len()
                );

                // Check if trimmed data contains meaningful content
                let is_meaningful = detect_meaningful_content(&trimmed);

                if trimmed.is_empty() || !is_meaningful {
                    actor_debug!("StateActor: Data not meaningful. Forcing Wakeup.");
                    (true, Vec::new())
                } else {
                    actor_debug!("StateActor: Content detected! Injecting.");
                    (false, trimmed)
                }
            };

            let operation_id = self.next_operation_id();

            #[cfg(target_arch = "wasm32")]
            {
                if let Some(handle) = self.pending_port_handle.clone() {
                    self.send_critical_port(PortMessage::Open {
                        port,
                        baud,
                        framing,
                        send_wakeup,
                        operation_id,
                        port_handle: handle,
                    })?;
                }
            }

            #[cfg(not(target_arch = "wasm32"))]
            {
                self.send_critical_port(PortMessage::Open {
                    port,
                    baud,
                    framing,
                    send_wakeup,
                    operation_id,
                })?;
            }

            // Inject the data captured during probing (CRITICAL - must arrive before user data)
            if !data_to_inject.is_empty() {
                self.send_critical_port(PortMessage::InjectData {
                    data: data_to_inject,
                })?;
            }
        } else {
            return Err(ActorError::Other(
                "No pending port for probe completion".into(),
            ));
        }

        Ok(())
    }
}
