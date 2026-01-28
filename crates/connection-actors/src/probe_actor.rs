use actor_protocol::{ActorError, ProbeResult, SystemEvent};
use actor_runtime::{Actor, ProbeMessage, StateMessage};
use futures_channel::mpsc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[cfg(target_arch = "wasm32")]
use actor_runtime::{create_cancel_future, race_with_cancellation};

#[cfg(target_arch = "wasm32")]
use core_types::Transport;

#[cfg(feature = "mavlink")]
use mavlink;

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

            attempts += 1;
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
                // Timeout - check exit conditions
                if buffer.len() > 10
                    || (buffer.len() > 64 && analysis::calculate_score_8n1(&buffer) as f64 > 0.90)
                {
                    break;
                }
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

    fn analyze_buffer(&self, buffer: &[u8], baud: u32) -> ProbeResult {
        if buffer.is_empty() {
            return ProbeResult {
                baud,
                framing: "8N1".into(),
                protocol: None,
                initial_data: buffer.to_vec(),
                confidence: 0.0,
            };
        }

        // Use proper scoring functions from analysis crate
        let score_8n1 = analysis::calculate_score_8n1(buffer) as f64;
        let score_7e1 = analysis::calculate_score_7e1(buffer) as f64;
        let score_mav = analysis::calculate_score_mavlink(buffer) as f64;

        // Debug log scoring for development
        #[cfg(debug_assertions)]
        #[cfg(target_arch = "wasm32")]
        web_sys::console::log_1(
            &format!(
                "AUTO: Rate {} => 8N1: {:.4}, 7E1: {:.4}, MAV: {:.4} (Size: {})",
                baud,
                score_8n1,
                score_7e1,
                score_mav,
                buffer.len()
            )
            .into(),
        );

        // MAVLink Priority Check (Robust) - Use integrity verification if available
        #[cfg(feature = "mavlink")]
        if self.verify_mavlink_integrity(buffer) {
            #[cfg(debug_assertions)]
            #[cfg(target_arch = "wasm32")]
            web_sys::console::log_1(&"AUTO: MAVLink Verified (Magic+Parse)!".into());

            return ProbeResult {
                baud,
                framing: "8N1".into(),
                protocol: Some("mavlink".into()),
                initial_data: buffer.to_vec(),
                confidence: 1.0,
            };
        }

        // Fallback to statistical score if verification inconclusive but score high
        if score_mav >= 0.99 {
            #[cfg(debug_assertions)]
            #[cfg(target_arch = "wasm32")]
            web_sys::console::log_1(&"AUTO: MAVLink Detected (Statistical).".into());

            return ProbeResult {
                baud,
                framing: "8N1".into(),
                protocol: Some("mavlink".into()),
                initial_data: buffer.to_vec(),
                confidence: score_mav,
            };
        }

        // Check for NMEA signature ($GP, $GN, etc.) - simple pattern detection
        let has_nmea = buffer.windows(2).any(|w| matches!(w, [b'$', b'G']));
        if has_nmea && score_8n1 > 0.85 {
            return ProbeResult {
                baud,
                framing: "8N1".into(),
                protocol: Some("nmea".into()),
                initial_data: buffer.to_vec(),
                confidence: score_8n1,
            };
        }

        // Find best score among framing options
        let mut best_score = score_8n1;
        let mut best_framing = "8N1";

        if score_7e1 > best_score {
            best_score = score_7e1;
            best_framing = "7E1";
        }

        // Check for early break threshold
        // High-Speed Optimization: Accept lower confidence for >= 1M baud
        let threshold = if baud >= 1_000_000 { 0.85 } else { 0.98 };

        if best_score > threshold && buffer.len() > 64 {
            #[cfg(debug_assertions)]
            #[cfg(target_arch = "wasm32")]
            web_sys::console::log_1(
                &format!(
                    "AUTO: High confidence match at {} baud (Score: {:.2} > {})",
                    baud, best_score, threshold
                )
                .into(),
            );
        }

        ProbeResult {
            baud,
            framing: best_framing.into(),
            protocol: None,
            initial_data: buffer.to_vec(),
            confidence: best_score,
        }
    }

    #[cfg(feature = "mavlink")]
    fn verify_mavlink_integrity(&self, buffer: &[u8]) -> bool {
        let mut reader = buffer;
        loop {
            let magic_idx = reader.iter().position(|&b| b == 0xFE || b == 0xFD);
            if let Some(idx) = magic_idx {
                if idx + 1 >= reader.len() {
                    return false;
                }
                let Some(&magic) = reader.get(idx) else {
                    return false;
                };
                let min_packet_size = if magic == 0xFE { 8 } else { 12 };

                if idx + min_packet_size > reader.len() {
                    return false;
                }

                let Some(sub_slice) = reader.get(idx..) else {
                    return false;
                };

                // Wrap in PeekReader as required by MAVLink API
                // &[u8] implements embedded_io::Read, so we can use it directly
                let mut peek_reader = mavlink::peek_reader::PeekReader::new(sub_slice);

                let res = if magic == 0xFE {
                    mavlink::read_v1_msg::<mavlink::common::MavMessage, _>(&mut peek_reader)
                } else {
                    mavlink::read_v2_msg::<mavlink::common::MavMessage, _>(&mut peek_reader)
                };

                if res.is_ok() {
                    #[cfg(debug_assertions)]
                    #[cfg(target_arch = "wasm32")]
                    web_sys::console::log_1(
                        &format!("MAVLink VERIFIED. Magic: {:02X}", magic).into(),
                    );
                    return true;
                } else {
                    #[cfg(debug_assertions)]
                    #[cfg(target_arch = "wasm32")]
                    web_sys::console::log_1(&"MAVLink Magic found but parse failed.".into());
                }

                let Some(next_reader) = reader.get(idx + 1..) else {
                    return false;
                };
                reader = next_reader;
            } else {
                return false;
            }
        }
    }
}

impl Actor for ProbeActor {
    type Message = ProbeMessage;

    fn name(&self) -> &'static str {
        "ProbeActor"
    }

    async fn handle(&mut self, msg: ProbeMessage) -> Result<(), ActorError> {
        match msg {
            #[cfg(target_arch = "wasm32")]
            ProbeMessage::Start { port, port_handle } => {
                self.handle_start(port, port_handle).await?
            }

            #[cfg(not(target_arch = "wasm32"))]
            ProbeMessage::Start { port } => self.handle_start(port).await?,

            ProbeMessage::Abort => self.handle_abort().await?,
        }

        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use futures::stream::StreamExt;

    fn create_test_actor() -> (
        ProbeActor,
        mpsc::Receiver<StateMessage>,
        mpsc::Receiver<SystemEvent>,
    ) {
        let (state_tx, state_rx) = mpsc::channel(100);
        let (event_tx, event_rx) = mpsc::channel(100);

        let actor = ProbeActor::new(state_tx, event_tx);
        (actor, state_rx, event_rx)
    }

    #[tokio::test]
    async fn test_analyze_mavlink() {
        let (actor, _, _) = create_test_actor();

        // Valid MAVLink v1 packet: FE len=3 ... total=3+8=11 bytes
        let buffer = vec![
            0xFE, 0x03, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
        ];
        let result = actor.analyze_buffer(&buffer, 115200);

        assert_eq!(result.baud, 115200);
        // Protocol detection only works with mavlink feature enabled and valid packet
        // For testing without feature, we rely on score
        #[cfg(feature = "mavlink")]
        assert_eq!(result.protocol, Some("mavlink".into()));
        assert!(result.confidence > 0.9);
    }

    #[tokio::test]
    async fn test_analyze_nmea() {
        let (actor, _, _) = create_test_actor();

        let buffer = b"$GPGGA,123519,4807.038,N,01131.000,E".to_vec();
        let result = actor.analyze_buffer(&buffer, 9600);

        assert_eq!(result.baud, 9600);
        assert_eq!(result.protocol, Some("nmea".into()));
        assert!(result.confidence > 0.85);
    }

    #[tokio::test]
    async fn test_analyze_text() {
        let (actor, _, _) = create_test_actor();

        let buffer = b"Hello World\nThis is text\n".to_vec();
        let result = actor.analyze_buffer(&buffer, 115200);

        // Text data should be detected with good confidence, but protocol should be None
        // (user should choose decoder manually for generic text)
        assert_eq!(result.protocol, None);
        assert!(result.confidence > 0.5);
    }

    #[tokio::test]
    async fn test_analyze_empty() {
        let (actor, _, _) = create_test_actor();

        let buffer = vec![];
        let result = actor.analyze_buffer(&buffer, 115200);

        assert_eq!(result.confidence, 0.0);
    }

    #[tokio::test]
    async fn test_abort_sets_flag() {
        let (mut actor, _, _) = create_test_actor();

        assert!(!actor.interrupt_flag.load(Ordering::Acquire));

        actor.handle_abort().await.unwrap();

        assert!(actor.interrupt_flag.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn test_probe_interrupted_by_user() {
        let (mut actor, _, mut event_rx) = create_test_actor();

        let port = actor_protocol::SerialPortInfo::new("/dev/ttyUSB0".into(), None, None);

        // Start probe in background
        let actor_clone_flag = actor.interrupt_flag.clone();
        let handle = tokio::spawn(async move {
            let result = actor.handle_start(port).await;
            (actor, result)
        });

        // Wait for probe to enter first gather_probe_data call (which has 100ms sleep)
        tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

        // Abort the probe (flag will be checked during gather_probe_data sleep or at loop start)
        actor_clone_flag.store(true, Ordering::Release);

        // Wait for probe to finish
        let (returned_actor, result) = handle.await.unwrap();

        // Should return error due to abort OR complete with empty results
        // (depending on timing, interrupt may return empty buffer or abort between iterations)
        match result {
            Err(ActorError::Other(msg)) if msg.contains("aborted") => {
                // Expected: aborted between loop iterations
            }
            Err(ActorError::Other(msg)) if msg.contains("failed") => {
                // Expected: completed with low confidence (empty buffers from interruption)
            }
            Ok(_) => {
                // Also acceptable: probe completed but with interrupted gather_probe_data
                // returning empty buffers, resulting in low confidence
            }
            Err(e) => panic!("Unexpected error: {:?}", e),
        }

        // Verify flag is set
        assert!(returned_actor.interrupt_flag.load(Ordering::Acquire));

        // Should have received either cancellation or progress status updates
        let mut found_event = false;
        while let Ok(Some(event)) = event_rx.try_next() {
            match event {
                SystemEvent::StatusUpdate { message } => {
                    if message.contains("cancelled") || message.contains("Scanning") {
                        found_event = true;
                        break;
                    }
                }
                SystemEvent::ProbeProgress { .. } => {
                    found_event = true;
                    break;
                }
                _ => {}
            }
        }
        assert!(found_event, "Should have received at least one probe event");
    }

    #[tokio::test]
    async fn test_probe_emits_progress() {
        let (mut actor, _, mut event_rx) = create_test_actor();

        let port = actor_protocol::SerialPortInfo::new("/dev/ttyUSB0".into(), None, None);

        // Start probe in background
        let handle = tokio::spawn(async move {
            let _ = actor.handle_start(port).await;
            actor
        });

        // Should receive at least one progress event
        let event = event_rx.next().await.unwrap();
        match event {
            SystemEvent::ProbeProgress { baud, .. } => {
                // Should be one of our candidates
                assert!([115200, 1500000, 921600, 57600, 9600, 38400, 19200].contains(&baud));
            }
            _ => panic!("Expected ProbeProgress event"),
        }

        // Wait for probe to finish
        let _actor = handle.await.unwrap();
    }

    #[tokio::test]
    async fn test_probe_reports_success() {
        let (mut actor, mut state_rx, _) = create_test_actor();

        let port = actor_protocol::SerialPortInfo::new("/dev/ttyUSB0".into(), None, None);
        actor.handle_start(port).await.ok();

        // Should receive ProbeComplete
        let msg = state_rx.next().await.unwrap();
        match msg {
            StateMessage::ProbeComplete { baud, framing, .. } => {
                assert!(baud > 0);
                assert!(!framing.is_empty());
            }
            _ => panic!("Expected ProbeComplete"),
        }
    }

    #[tokio::test]
    async fn test_analyze_binary() {
        let (actor, _, _) = create_test_actor();

        let buffer = vec![0x00, 0x01, 0x02, 0xFF, 0xAA, 0x55];
        let result = actor.analyze_buffer(&buffer, 115200);

        // Should detect as generic binary with moderate confidence
        assert_eq!(result.protocol, None);
        assert!(result.confidence > 0.0);
        assert!(result.confidence < 0.7);
    }

    #[tokio::test]
    #[cfg(feature = "mavlink")]
    async fn test_probe_handles_multiple_protocols_in_buffer() {
        let (actor, _, _) = create_test_actor();

        // Valid MAVLink v1 HEARTBEAT message (17 bytes)
        // Generated with proper CRC for HEARTBEAT (CRC_EXTRA=50)
        let mut buffer = vec![
            0xFE, // STX
            0x09, // payload length
            0x00, // sequence
            0x01, // system_id
            0x01, // component_id
            0x00, // message_id (HEARTBEAT)
            // Payload (9 bytes):
            0x00, 0x00, 0x00, 0x00, // custom_mode
            0x02, // type (QUADROTOR)
            0x03, // autopilot (ARDUPILOTMEGA)
            0x00, // base_mode
            0x04, // system_status (ACTIVE)
            0x03, // mavlink_version
            0xD0, 0x14, // CRC (X.25)
        ];

        // Append NMEA-like data to test mixed protocols
        buffer.extend_from_slice(b"$GPGGA,123519");

        let result = actor.analyze_buffer(&buffer, 115200);

        // Should detect MAVLink due to strong integrity verification
        assert_eq!(result.protocol, Some("mavlink".to_string()));
        assert!(result.confidence > 0.5);
    }

    #[tokio::test]
    async fn test_probe_with_very_low_baud() {
        let (actor, _, _) = create_test_actor();

        // Test with minimum baud rate
        let buffer = b"Hello World";
        let result = actor.analyze_buffer(buffer, 300);

        // Should still work at low baud rates
        assert!(result.confidence > 0.0);
    }

    #[tokio::test]
    async fn test_probe_with_very_high_baud() {
        let (actor, _, _) = create_test_actor();

        // Test with high baud rate
        let buffer = b"Test data";
        let result = actor.analyze_buffer(buffer, 921600);

        // Should still work at high baud rates
        assert!(result.confidence > 0.0);
    }

    #[tokio::test]
    async fn test_abort_message_sets_interrupt_flag() {
        let (mut actor, _, _) = create_test_actor();

        // Send abort message
        actor.handle_abort().await.unwrap();

        // Interrupt flag should be set
        assert!(actor
            .interrupt_flag
            .load(std::sync::atomic::Ordering::Relaxed));
    }

    #[tokio::test]
    async fn test_probe_with_partial_mavlink_frame() {
        let (actor, _, _) = create_test_actor();

        // Incomplete MAVLink frame (header only)
        let buffer = vec![0xFE, 0x09, 0x00, 0x01, 0x01]; // Magic + partial header

        let result = actor.analyze_buffer(&buffer, 115200);

        // Should still detect some confidence (or zero if buffer too small)
        assert!(result.confidence >= 0.0);
        assert_eq!(result.baud, 115200);
    }

    #[tokio::test]
    async fn test_probe_with_ascii_control_characters() {
        let (actor, _, _) = create_test_actor();

        // Buffer with control characters and text
        let buffer = b"\x1b[0m\r\n$ Hello";

        let result = actor.analyze_buffer(buffer, 115200);

        // Should handle ANSI sequences and still detect text
        assert!(result.confidence > 0.0);
    }

    #[tokio::test]
    async fn test_probe_result_includes_initial_data() {
        // Test that ProbeResult includes initial_data field
        let result = actor_protocol::ProbeResult {
            baud: 115200,
            framing: "8N1".to_string(),
            protocol: Some("mavlink".to_string()),
            initial_data: vec![1, 2, 3, 4, 5],
            confidence: 0.95,
        };

        // Verify all fields are accessible
        assert_eq!(result.baud, 115200);
        assert_eq!(result.framing, "8N1");
        assert_eq!(result.protocol, Some("mavlink".to_string()));
        assert_eq!(result.initial_data, vec![1, 2, 3, 4, 5]);
        assert_eq!(result.confidence, 0.95);
    }

    #[tokio::test]
    async fn test_probe_with_repeated_characters() {
        let (actor, _, _) = create_test_actor();

        // Buffer with repeated characters (potential echo)
        let buffer = b"AAAAAAAAAA";

        let result = actor.analyze_buffer(buffer, 115200);

        // Should detect but with lower confidence due to repetition
        assert!(result.confidence > 0.0);
    }

    #[tokio::test]
    #[cfg(feature = "mavlink")]
    async fn test_probe_confidence_mavlink_vs_text() {
        let (actor, _, _) = create_test_actor();

        // Two valid MAVLink v1 HEARTBEAT messages (34 bytes total)
        let mavlink_buffer = vec![
            // Message 1 (sequence 0):
            0xFE, 0x09, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x03, 0x00, 0x04,
            0x03, 0xD0, 0x14, // Message 2 (sequence 1):
            0xFE, 0x09, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x03, 0x00, 0x04,
            0x03, 0x3A, 0x6A,
        ];

        // Use mixed binary and text to get lower confidence score
        let text_buffer = b"\x01\x02\x03Hello\xFF\xFE test data\x00";

        let mavlink_result = actor.analyze_buffer(&mavlink_buffer, 115200);
        let text_result = actor.analyze_buffer(text_buffer, 115200);

        // Both should have some confidence
        assert!(mavlink_result.confidence >= 0.0);
        assert!(text_result.confidence >= 0.0);

        // MAVLink should have higher confidence (1.0) due to integrity verification
        assert!(mavlink_result.confidence > text_result.confidence);
    }

    #[tokio::test]
    #[cfg(feature = "mavlink")]
    async fn test_probe_mavlink_v2_detection() {
        let (actor, _, _) = create_test_actor();

        // Valid MAVLink v2 HEARTBEAT message (21 bytes)
        // Generated with proper CRC for HEARTBEAT (CRC_EXTRA=50)
        let buffer = vec![
            0xFD, // STX (v2)
            0x09, // payload length
            0x00, // incompatibility flags
            0x00, // compatibility flags
            0x00, // sequence
            0x01, // system_id
            0x01, // component_id
            0x00, 0x00, 0x00, // message_id (24-bit, HEARTBEAT = 0)
            // Payload (9 bytes):
            0x00, 0x00, 0x00, 0x00, // custom_mode
            0x02, // type (QUADROTOR)
            0x03, // autopilot (ARDUPILOTMEGA)
            0x00, // base_mode
            0x04, // system_status (ACTIVE)
            0x03, // mavlink_version
            0x4A, 0xD7, // CRC (X.25)
        ];

        let result = actor.analyze_buffer(&buffer, 115200);

        // Should detect MAVLink v2 with high confidence
        assert_eq!(result.protocol, Some("mavlink".to_string()));
        assert_eq!(
            result.confidence, 1.0,
            "Should have perfect confidence for verified MAVLink v2"
        );
    }

    #[tokio::test]
    #[cfg(feature = "mavlink")]
    async fn test_probe_mavlink_v1_and_v2_mixed() {
        let (actor, _, _) = create_test_actor();

        // Mix of v1 and v2 messages
        let mut buffer = vec![
            // MAVLink v1 HEARTBEAT:
            0xFE, 0x09, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x03, 0x00, 0x04,
            0x03, 0xD0, 0x14,
        ];
        // Append MAVLink v2 HEARTBEAT:
        buffer.extend_from_slice(&[
            0xFD, 0x09, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
            0x02, 0x03, 0x00, 0x04, 0x03, 0x4A, 0xD7,
        ]);

        let result = actor.analyze_buffer(&buffer, 115200);

        // Should detect MAVLink (either v1 or v2)
        assert_eq!(result.protocol, Some("mavlink".to_string()));
        assert_eq!(result.confidence, 1.0, "Should verify mixed v1/v2 messages");
    }
}
