use actor_protocol::{ActorError, ProbeResult, SystemEvent};
use actor_runtime::StateMessage;
use futures_channel::mpsc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[cfg(target_arch = "wasm32")]
use actor_runtime::{create_cancel_future, race_with_cancellation};

#[cfg(target_arch = "wasm32")]
use core_types::Transport;

// Import centralized constants
use crate::constants::BAUD_CANDIDATES;

#[cfg(target_arch = "wasm32")]
use crate::constants;

/// ProbeActor performs auto-detection of baud rate and protocols
///
/// Responsibilities:
/// - Test multiple baud rates to find optimal configuration
/// - Detect framing (8N1, 8E1, etc.)
/// - Identify protocols (MAVLink, NMEA, etc.) via heuristics
/// - Support interruptible probing
/// - Report progress to UI
pub struct ProbeActor {
    state_tx: mpsc::Sender<StateMessage>,
    event_tx: mpsc::Sender<SystemEvent>,
    interrupt_flag: Arc<AtomicBool>,

    // Store port handle (WASM-only) - replaces PENDING_PORT global
    #[cfg(target_arch = "wasm32")]
    port_handle: Option<actor_runtime::channels::PortHandle>,
}

impl ProbeActor {
    pub fn new(state_tx: mpsc::Sender<StateMessage>, event_tx: mpsc::Sender<SystemEvent>) -> Self {
        Self {
            state_tx,
            event_tx,
            interrupt_flag: Arc::new(AtomicBool::new(false)),
            #[cfg(target_arch = "wasm32")]
            port_handle: None,
        }
    }

    async fn handle_start(
        &mut self,
        port: actor_protocol::SerialPortInfo,
        #[cfg(target_arch = "wasm32")] port_handle: actor_runtime::channels::PortHandle,
    ) -> Result<(), ActorError> {
        // Store port handle for use in gather_probe_data
        #[cfg(target_arch = "wasm32")]
        {
            self.port_handle = Some(port_handle);
        }
        self.interrupt_flag.store(false, Ordering::Release);

        let mut best_result = ProbeResult::default();
        let mut best_score = 0.0;

        let mut last_error: Option<String> = None;

        for &baud in BAUD_CANDIDATES {
            // Check for interruption
            if self.interrupt_flag.load(Ordering::Acquire) {
                let _ = self.event_tx.try_send(SystemEvent::StatusUpdate {
                    message: "Auto-detection cancelled".into(),
                });
                return Err(ActorError::Other("Probe aborted".into()));
            }

            // Emit progress
            let _ = self.event_tx.try_send(SystemEvent::ProbeProgress {
                baud,
                message: format!("Scanning {} baud...", baud),
            });

            // Simulate gathering data at this baud rate
            // Refined Error Handling:
            // - AlreadyOpen/InvalidState: Fail Fast (User needs to fix setup)
            // - Other (ConnectionFailed): Resilience (Wait 500ms, try next)
            let buffer = match self.gather_probe_data(&port, baud).await {
                Ok(b) => b,
                Err(ActorError::Transport(msg)) => {
                    // Check for fatal errors
                    if msg.contains("AlreadyOpen") || msg.contains("InvalidState") {
                        // Fail immediately - CRITICAL coordination message
                        self.state_tx
                            .try_send(StateMessage::ConnectionFailed {
                                reason: format!("Probe halted: {}", msg),
                            })
                            .map_err(|_| {
                                ActorError::ChannelClosed(
                                    "StateActor unavailable during probe fatal error".into(),
                                )
                            })?;
                        return Ok(()); // Stop actor gracefully
                    }

                    // Transient/Network error (Unplugged, Busy-but-generic)
                    #[cfg(target_arch = "wasm32")]
                    {
                        #[cfg(debug_assertions)]
                        web_sys::console::warn_1(
                            &format!("Probe: Error at {} baud: {}. Retrying next...", baud, msg)
                                .into(),
                        );
                        gloo_timers::future::sleep(std::time::Duration::from_millis(500)).await;
                    }

                    last_error = Some(msg);
                    continue; // Skip analysis, try next baud
                }
                Err(e) => return Err(e), // Critical actor error
            };

            // Analyze buffer
            let result = self.analyze_buffer(&buffer, baud);

            if result.confidence > best_score {
                best_score = result.confidence;
                best_result = result;
            }

            // Early break on perfect match or high confidence
            // Optimization: If "Perfect" match found, stop scanning remaining rates
            if best_score > 0.99 && buffer.len() > 64 {
                #[cfg(debug_assertions)]
                #[cfg(target_arch = "wasm32")]
                web_sys::console::log_1(
                    &format!("AUTO: Perfect match found at {}. Stopping scan.", baud).into(),
                );
                break;
            }

            // High-Speed Optimization: Accept lower confidence for >= 1M baud
            let threshold = if baud >= 1_000_000 { 0.85 } else { 0.98 };
            if best_score > threshold {
                #[cfg(debug_assertions)]
                #[cfg(target_arch = "wasm32")]
                web_sys::console::log_1(
                    &format!(
                        "AUTO: Early Break at {} baud (Score: {:.2} > {})",
                        baud, best_score, threshold
                    )
                    .into(),
                );
                break;
            }
        }

        // Report result to StateActor
        if best_score > 0.3 {
            #[cfg(debug_assertions)]
            #[cfg(target_arch = "wasm32")]
            web_sys::console::log_1(
                &format!(
                    "AUTO: FINAL SELECTION => {} baud (Score: {:.4}, Protocol: {:?})",
                    best_result.baud, best_score, best_result.protocol
                )
                .into(),
            );

            self.state_tx
                .try_send(StateMessage::ProbeComplete {
                    baud: best_result.baud,
                    framing: best_result.framing.clone(),
                    protocol: best_result.protocol.clone(),
                    initial_data: best_result.initial_data.clone(),
                })
                .map_err(|_| {
                    ActorError::ChannelClosed("StateActor unavailable during ProbeComplete".into())
                })?;
        } else {
            #[cfg(debug_assertions)]
            #[cfg(target_arch = "wasm32")]
            web_sys::console::log_1(
                &format!(
                    "AUTO: FAILED - best score was {:.4} (threshold: 0.3)",
                    best_score
                )
                .into(),
            );

            // Failed to detect
            let reason = if let Some(err) = last_error {
                format!("Auto-detection passed with errors. Last error: {}", err)
            } else {
                "Auto-detection failed: no valid signal detected".into()
            };

            // CRITICAL coordination message - must succeed
            self.state_tx
                .try_send(StateMessage::ConnectionFailed { reason })
                .map_err(|_| {
                    ActorError::ChannelClosed("StateActor unavailable during probe failure".into())
                })?;
        }

        Ok(())
    }

    async fn handle_abort(&mut self) -> Result<(), ActorError> {
        self.interrupt_flag.store(true, Ordering::Release);
        Ok(())
    }

    /// Attempts to open port with retry logic
    ///
    /// Returns Ok(transport) on success, Err on fatal error.
    /// Returns Err with "Interrupted" if cancelled.
    #[cfg(target_arch = "wasm32")]
    async fn open_port_with_retry(
        &self,
        port: web_sys::SerialPort,
        config: core_types::SerialConfig,
    ) -> Result<transport_webserial::WebSerialTransport, ActorError> {
        let mut transport = transport_webserial::WebSerialTransport::new();
        let mut attempts = 0;

        while attempts < constants::probe::PORT_OPEN_MAX_RETRIES {
            // Check for interruption before each attempt
            if self.interrupt_flag.load(Ordering::Acquire) {
                return Err(ActorError::Other("Interrupted during port open".into()));
            }

            // Race port.open() against cancellation
            let open_result = race_with_cancellation(
                transport.open(port.clone(), config.clone()),
                self.interrupt_flag.clone(),
            )
            .await;

            match open_result {
                Some(Ok(_)) => {
                    // Success - verify not interrupted immediately after
                    if self.interrupt_flag.load(Ordering::Acquire) {
                        return Err(ActorError::Other("Interrupted after port open".into()));
                    }
                    return Ok(transport);
                }
                Some(Err(core_types::TransportError::ConnectionFailed(_)))
                    if attempts < constants::probe::PORT_OPEN_MAX_RETRIES - 1 =>
                {
                    // Retry on generic connection failure
                    attempts += 1;

                    let sleep_result = race_with_cancellation(
                        gloo_timers::future::sleep(std::time::Duration::from_millis(
                            constants::probe::PORT_OPEN_RETRY_DELAY_MS,
                        )),
                        self.interrupt_flag.clone(),
                    )
                    .await;

                    if sleep_result.is_none() {
                        return Err(ActorError::Other("Interrupted during retry delay".into()));
                    }
                }
                Some(Err(e)) => {
                    // Fatal error - propagate immediately
                    return Err(ActorError::Transport(format!("Failed to open port: {}", e)));
                }
                None => {
                    // Cancelled during open
                    return Err(ActorError::Other("Interrupted during port open".into()));
                }
            }
        }

        Err(ActorError::Transport(
            "Max retries exceeded opening port".into(),
        ))
    }

    /// Reads data from port with timeout and cancellation support
    ///
    /// Returns buffer of received bytes, or empty buffer if interrupted.
    #[cfg(target_arch = "wasm32")]
    async fn read_data_with_timeout(
        &self,
        transport: &mut transport_webserial::WebSerialTransport,
    ) -> Result<Vec<u8>, ActorError> {
        use futures::future::{select, Either};

        let mut buffer = Vec::new();

        // Send single wakeup to trigger device response
        let write_result =
            race_with_cancellation(transport.write(b"\r"), self.interrupt_flag.clone()).await;

        if write_result.is_none() {
            return Ok(Vec::new()); // Interrupted
        }

        let start = js_sys::Date::now();
        let mut max_time = constants::probe::INITIAL_READ_TIMEOUT_MS;

        loop {
            let elapsed = js_sys::Date::now() - start;
            if elapsed > max_time {
                break;
            }

            let remaining = (max_time - elapsed).max(10.0) as i32;

            // Create timeout future
            let timeout_fut = async {
                let promise = js_sys::Promise::new(&mut |r, _| {
                    if let Some(window) = web_sys::window() {
                        let _ = window
                            .set_timeout_with_callback_and_timeout_and_arguments_0(&r, remaining);
                    }
                });
                let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
                None
            };

            // Create read future
            let read_fut = async { transport.read_chunk().await.ok() };

            // Race: Read vs Timeout vs Cancellation (3-way race)
            let race1 = select(Box::pin(read_fut), Box::pin(timeout_fut));
            let cancel_fut = create_cancel_future(self.interrupt_flag.clone());

            let result = match select(Box::pin(race1), Box::pin(cancel_fut)).await {
                Either::Left((Either::Left((res, _)), _)) => res, // Read finished
                Either::Left((Either::Right((res, _)), _)) => res, // Timeout finished
                Either::Right(_) => break,                        // Cancelled
            };

            // Check interruption after await
            if self.interrupt_flag.load(Ordering::Acquire) {
                break;
            }

            match result {
                Some((bytes, _ts)) => {
                    if !bytes.is_empty() {
                        buffer.extend_from_slice(&bytes);

                        if buffer.len() > 200 {
                            break;
                        }

                        // Extend timeout after receiving data
                        max_time = constants::probe::EXTENDED_READ_TIMEOUT_MS;
                    }
                }
                None => break, // Timeout
            }

            // Safety exit on high confidence
            if buffer.len() > 64 && analysis::calculate_score_8n1(&buffer) as f64 > 0.90 {
                break;
            }
        }

        Ok(buffer)
    }

    /// Closes port and performs mandatory cooldown
    ///
    /// Returns Err if interrupted during critical cleanup.
    #[cfg(target_arch = "wasm32")]
    async fn close_port_with_cooldown(
        &self,
        transport: &mut transport_webserial::WebSerialTransport,
    ) -> Result<(), ActorError> {
        // Race close against cancellation
        let close_result =
            race_with_cancellation(transport.close(), self.interrupt_flag.clone()).await;

        if close_result.is_some() && self.interrupt_flag.load(Ordering::Acquire) {
            return Err(ActorError::Other("Interrupted after port close".into()));
        }

        // Mandatory cooldown to avoid port lock issues
        let cooldown_result = race_with_cancellation(
            gloo_timers::future::sleep(std::time::Duration::from_millis(
                constants::port::CLOSE_COOLDOWN_MS,
            )),
            self.interrupt_flag.clone(),
        )
        .await;

        if cooldown_result.is_none() {
            return Err(ActorError::Other("Interrupted during cooldown".into()));
        }

        // Final interruption check
        if self.interrupt_flag.load(Ordering::Acquire) {
            return Err(ActorError::Other("Interrupted after cleanup".into()));
        }

        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    async fn gather_probe_data(
        &self,
        _port: &actor_protocol::SerialPortInfo,
        baud: u32,
    ) -> Result<Vec<u8>, ActorError> {
        // Early exit if already interrupted
        if self.interrupt_flag.load(Ordering::Acquire) {
            return Ok(Vec::new());
        }

        // Get port from stored handle
        let port = self
            .port_handle
            .as_ref()
            .ok_or_else(|| {
                ActorError::Transport(
                    "Port handle unavailable for probing - ensure Connect was called with port"
                        .to_string(),
                )
            })?
            .as_ref()
            .clone();

        // Create transport and config
        let config = core_types::SerialConfig {
            baud_rate: baud,
            data_bits: 8,
            parity: "none".into(),
            stop_bits: 1,
            flow_control: "none".into(),
        };

        // Open port with retry logic
        let mut transport = self.open_port_with_retry(port, config).await?;

        // Read data with timeout
        let buffer = self.read_data_with_timeout(&mut transport).await?;

        // Close port and perform cooldown
        self.close_port_with_cooldown(&mut transport).await?;

        Ok(buffer)
    }

    #[cfg(not(target_arch = "wasm32"))]
    async fn gather_probe_data(
        &self,
        _port: &actor_protocol::SerialPortInfo,
        _baud: u32,
    ) -> Result<Vec<u8>, ActorError> {
        // Check for interruption (important for testing)
        if self.interrupt_flag.load(Ordering::Acquire) {
            return Ok(Vec::new());
        }

        // Simulate some async work to allow interruption (100ms to ensure test can set flag)
        // In tests, use tokio (available as dev-dependency)
        // In production native builds, no delay needed (stub returns immediately)
        #[cfg(test)]
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;

        // Check again after sleep
        if self.interrupt_flag.load(Ordering::Acquire) {
            return Ok(Vec::new());
        }

        // Native stub for testing - return simulated data
        Ok(vec![0x55, 0xAA, 0x55, 0xAA])
    }
}

mod actor_impl;
mod classifier;

#[cfg(test)]
mod tests;
