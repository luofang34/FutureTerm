use actor_protocol::{ActorError, ConnectionState, SystemEvent, UiCommand};
use actor_runtime::{actor_debug, actor_info, Actor, PortMessage, StateMessage};

impl Actor for super::StateActor {
    type Message = StateMessage;

    fn name(&self) -> &'static str {
        "StateActor"
    }

    async fn handle(&mut self, msg: StateMessage) -> Result<(), ActorError> {
        match msg {
            // UiCommand with port handle (WASM-only)
            #[cfg(target_arch = "wasm32")]
            StateMessage::UiCommandWithPort { cmd, port_handle } => match cmd {
                UiCommand::Connect {
                    port,
                    baud,
                    framing,
                } => {
                    self.handle_connect(port, baud, framing, Some(port_handle))
                        .await?
                }
                UiCommand::Reconfigure {
                    baud,
                    framing,
                    active_framing,
                } => {
                    self.handle_reconfigure_with_port(
                        baud,
                        framing,
                        active_framing,
                        Some(port_handle),
                    )
                    .await?
                }
                UiCommand::Disconnect => self.handle_disconnect().await?,
                UiCommand::SetDecoder { .. } | UiCommand::SetFramer { .. } => {
                    // These are handled by worker, not state machine
                }
                UiCommand::WriteData { .. } => {
                    // This should be routed directly to PortActor
                    // Not handled by StateActor
                }
            },

            // UiCommand without port handle
            StateMessage::UiCommand(cmd) => match cmd {
                UiCommand::Connect {
                    port,
                    baud,
                    framing,
                } => {
                    #[cfg(target_arch = "wasm32")]
                    {
                        self.handle_connect(port, baud, framing, None).await?
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    {
                        self.handle_connect(port, baud, framing).await?
                    }
                }
                UiCommand::Disconnect => self.handle_disconnect().await?,
                UiCommand::Reconfigure {
                    baud,
                    framing,
                    active_framing,
                } => {
                    #[cfg(target_arch = "wasm32")]
                    {
                        self.handle_reconfigure_with_port(baud, framing, active_framing, None)
                            .await?
                    }
                    #[cfg(not(target_arch = "wasm32"))]
                    {
                        self.handle_reconfigure(baud, framing, active_framing)
                            .await?
                    }
                }
                UiCommand::SetDecoder { .. } | UiCommand::SetFramer { .. } => {
                    // These are handled by worker, not state machine
                }
                UiCommand::WriteData { .. } => {
                    // This should be routed directly to PortActor
                    // Not handled by StateActor
                }
            },
            StateMessage::ConnectionEstablished { operation_id } => {
                self.handle_connection_established(operation_id).await?
            }
            StateMessage::ConnectionFailed { reason } => {
                self.handle_connection_failed(reason).await?
            }
            StateMessage::ConnectionLost => {
                // Device disconnected - close the port cleanly before transitioning (CRITICAL)
                self.send_critical_port(PortMessage::Close)?;

                // Transition to DeviceLost state (ready for auto-reconnect)
                self.transition(ConnectionState::DeviceLost)?;
            }
            StateMessage::ConnectionClosed => {
                // PortActor has confirmed port is fully closed
                if self.state == ConnectionState::Disconnecting {
                    // Check if this is a reconfigure operation
                    #[cfg(target_arch = "wasm32")]
                    {
                        if self.pending_reconfigure_baud.is_some() {
                            // Complete reconfiguration (reconnect with new settings)
                            self.transition(ConnectionState::Disconnected)?;
                            actor_debug!("StateActor: Port closed, completing reconfiguration");
                            self.complete_reconfigure().await?;
                            return Ok(());
                        }
                    }

                    // Normal disconnect (no reconfigure)
                    self.transition(ConnectionState::Disconnected)?;
                    actor_debug!("StateActor: Port close confirmed, transitioned to Disconnected");
                } else {
                    actor_debug!(
                        "StateActor: Ignoring ConnectionClosed in {:?} state",
                        self.state
                    );
                }
            }
            StateMessage::ProbeComplete {
                baud,
                framing,
                protocol,
                initial_data,
            } => {
                self.handle_probe_complete(baud, framing, protocol, initial_data)
                    .await?
            }
            #[cfg(target_arch = "wasm32")]
            StateMessage::DeviceReappeared { port, port_handle } => {
                // Only reconnect from DeviceLost state (not from other states)
                if self.state != ConnectionState::DeviceLost {
                    actor_debug!("Ignoring DeviceReappeared in {:?} state", self.state);
                    return Ok(());
                }

                // Transition to AutoReconnecting
                self.transition(ConnectionState::AutoReconnecting)?;

                // Brief delay for port to stabilize (reduced since ReconnectActor already waited)
                use wasm_bindgen_futures::JsFuture;
                let _ = JsFuture::from(js_sys::Promise::new(&mut |resolve, _| {
                    if let Some(window) = web_sys::window() {
                        let _ = window
                            .set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 50);
                    }
                }))
                .await;

                // Determine baud rate: use stored rate, or default to 115200
                let baud = if self.pending_baud > 0 {
                    self.pending_baud
                } else {
                    115200
                };

                actor_debug!("Auto-reconnecting at {} baud", baud);

                let operation_id = self.next_operation_id();

                // Trigger actual reconnection by opening the port (CRITICAL)
                // Note: AutoReconnecting → Connected transition happens when PortActor confirms success
                self.send_critical_port(PortMessage::Open {
                    port,
                    baud,
                    framing: "8N1".into(),
                    send_wakeup: false, // Auto-reconnect: don't send wakeup byte
                    operation_id,
                    port_handle,
                })?;
            }

            #[cfg(not(target_arch = "wasm32"))]
            StateMessage::DeviceReappeared { port } => {
                // Only reconnect from DeviceLost state (not from other states)
                if self.state != ConnectionState::DeviceLost {
                    return Ok(());
                }

                // Transition to AutoReconnecting
                self.transition(ConnectionState::AutoReconnecting)?;

                // Determine baud rate: use stored rate, or default to 115200
                let baud = if self.pending_baud > 0 {
                    self.pending_baud
                } else {
                    115200
                };

                let operation_id = self.next_operation_id();

                // Trigger actual reconnection by opening the port (CRITICAL)
                self.send_critical_port(PortMessage::Open {
                    port,
                    baud,
                    framing: "8N1".into(),
                    send_wakeup: false,
                    operation_id,
                })?;
            }

            StateMessage::OperationTimeout {
                operation,
                state: expected_state,
            } => {
                // Only handle timeout if we're still in the expected state
                // (if state has changed, the operation already completed)
                if self.state != expected_state {
                    actor_debug!(
                        "Ignoring {} timeout - already transitioned to {:?}",
                        operation,
                        self.state
                    );
                    return Ok(());
                }

                actor_info!("Operation timeout: {} in state {:?}", operation, self.state);

                // Send error event to UI (non-critical)
                let error_msg = format!("{} operation timed out. Please try again.", operation);
                self.send_ui_event(SystemEvent::Error { message: error_msg });

                // Transition to safe state based on current state
                match self.state {
                    ConnectionState::Probing
                    | ConnectionState::Connecting
                    | ConnectionState::Reconfiguring
                    | ConnectionState::AutoReconnecting => {
                        // Failed to establish connection - go to Disconnected (CRITICAL operations)
                        self.send_critical_port(PortMessage::Close)?;
                        self.transition(ConnectionState::Disconnecting)?;
                        // Note: Will transition to Disconnected when ConnectionClosed arrives
                    }
                    ConnectionState::Disconnecting => {
                        // Force transition to Disconnected even if close didn't confirm
                        actor_info!("Forcing transition to Disconnected after disconnect timeout");
                        self.transition(ConnectionState::Disconnected)?;
                    }
                    _ => {
                        // Shouldn't happen (timeout only spawned for specific states)
                        actor_info!("Unexpected timeout in state {:?}", self.state);
                    }
                }
            }
        }

        Ok(())
    }
}
