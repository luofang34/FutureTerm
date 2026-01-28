use actor_protocol::SystemEvent;
use actor_runtime::{actor_debug, StateMessage};
use futures_channel::mpsc;

#[cfg(target_arch = "wasm32")]
use crate::constants;

#[cfg(target_arch = "wasm32")]
use {
    actor_protocol::ActorError,
    actor_runtime::{Actor, PortMessage},
    core_types::{SerialConfig, Transport},
    futures::{future::FutureExt, stream::StreamExt},
    std::time::Duration,
    transport_webserial::WebSerialTransport,
    wasm_bindgen_futures::spawn_local,
};

#[cfg(target_arch = "wasm32")]
mod wasm_port_actor {
    use super::*;

    /// Wrapper for Rc<WebSerialTransport> that's Send in WASM (single-threaded)
    ///
    /// SAFETY: SendableTransport is safe to Send ONLY in single-threaded WASM.
    ///
    /// This wrapper makes Rc<WebSerialTransport> Send to satisfy actor message passing.
    /// Rc is !Send by default because it uses non-atomic reference counting, which
    /// would cause data races in true multi-threaded environments.
    ///
    /// **Resource cleanup**: When Rc::try_unwrap() fails (multiple references exist),
    /// we rely on the WebSerialTransport Drop implementation to clean up resources
    /// when the last reference is dropped. This is safe because:
    /// 1. PortActor's transport reference is dropped in handle_close()
    /// 2. Read loop's reference is dropped when loop exits
    /// 3. Drop implementation spawns async cleanup in background
    ///
    /// However, in single-threaded WASM:
    /// 1. All operations execute on the main thread via spawn_local (no parallelism)
    /// 2. Rc operations are sequentially consistent within the single thread
    /// 3. The "Send" occurs via message passing but execution remains single-threaded
    ///
    /// If atomics feature is enabled, compilation MUST fail to prevent UB.
    #[derive(Clone)]
    pub(super) struct SendableTransport(pub(super) std::rc::Rc<WebSerialTransport>);

    // Compile-time safety check: prevent SendableTransport with WASM atomics
    #[cfg(all(target_arch = "wasm32", feature = "atomics"))]
    compile_error!(
        "SendableTransport is unsafe with WASM atomics! \
         Rc uses non-atomic reference counting which causes data races in multi-threaded WASM. \
         Use Arc<Mutex<WebSerialTransport>> instead if you need thread-safety."
    );

    #[cfg(all(target_arch = "wasm32", not(feature = "atomics")))]
    unsafe impl Send for SendableTransport {}

    impl std::ops::Deref for SendableTransport {
        type Target = WebSerialTransport;
        fn deref(&self) -> &Self::Target {
            &self.0
        }
    }
}

#[cfg(target_arch = "wasm32")]
use wasm_port_actor::SendableTransport;

#[cfg(not(target_arch = "wasm32"))]
type SendableTransport = ();

/// PortActor manages serial port I/O operations
///
/// Responsibilities:
/// - Open/close serial ports
/// - Manage read loop for incoming data
/// - Handle write requests
/// - Retry logic for port opening
/// - Notify StateActor of connection events
pub struct PortActor {
    active_port: Option<String>, // Port path
    transport: Option<SendableTransport>,
    state_tx: mpsc::Sender<StateMessage>,
    event_tx: mpsc::Sender<SystemEvent>,
    /// Current operation ID (assigned by StateActor, echoed back in ConnectionEstablished)
    current_operation_id: Option<u32>,

    #[cfg(target_arch = "wasm32")]
    shutdown_tx: Option<mpsc::Sender<()>>,

    #[cfg(target_arch = "wasm32")]
    done_rx: Option<futures_channel::oneshot::Receiver<()>>,
}

impl PortActor {
    pub fn new(state_tx: mpsc::Sender<StateMessage>, event_tx: mpsc::Sender<SystemEvent>) -> Self {
        Self {
            active_port: None,
            transport: None,
            state_tx,
            event_tx,
            current_operation_id: None,

            #[cfg(target_arch = "wasm32")]
            shutdown_tx: None,

            #[cfg(target_arch = "wasm32")]
            done_rx: None,
        }
    }

    #[cfg(target_arch = "wasm32")]
    fn parse_framing(framing: &str, baud: u32) -> Result<SerialConfig, String> {
        // Parse framing string like "8N1" (8 data bits, No parity, 1 stop bit)

        // Validate format: must be exactly 3 characters
        if framing.len() != 3 {
            return Err(format!(
                "Invalid framing format '{}': must be 3 characters (e.g., '8N1')",
                framing
            ));
        }

        let chars: Vec<char> = framing.chars().collect();

        // Parse data_bits (first character)
        let data_bits = match chars.first() {
            Some('7') => 7,
            Some('8') => 8,
            Some(c) => return Err(format!("Invalid data bits '{}': must be 7 or 8", c)),
            None => return Err("Internal error: framing string unexpectedly empty".into()),
        };

        // Parse parity (second character, case insensitive)
        let parity = match chars.get(1).map(|c| c.to_ascii_uppercase()) {
            Some('N') => "none",
            Some('E') => "even",
            Some('O') => "odd",
            Some(c) => return Err(format!("Invalid parity '{}': must be N, E, or O", c)),
            None => return Err("Internal error: missing parity character".into()),
        };

        // Parse stop_bits (third character)
        let stop_bits = match chars.get(2) {
            Some('1') => 1,
            Some('2') => 2,
            Some(c) => return Err(format!("Invalid stop bits '{}': must be 1 or 2", c)),
            None => return Err("Internal error: missing stop bits character".into()),
        };

        Ok(SerialConfig {
            baud_rate: baud,
            data_bits,
            flow_control: "none".into(),
            parity: parity.into(),
            stop_bits,
        })
    }

    #[cfg(target_arch = "wasm32")]
    async fn handle_open(
        &mut self,
        port_info: actor_protocol::SerialPortInfo,
        baud: u32,
        framing: String,
        send_wakeup: bool,
        operation_id: u32,
        #[cfg(target_arch = "wasm32")] port_handle: actor_runtime::channels::PortHandle,
    ) -> Result<(), ActorError> {
        if self.active_port.is_some() {
            return Err(ActorError::InvalidTransition(
                "Port already open".to_string(),
            ));
        }

        // Store operation ID to echo back in ConnectionEstablished
        // StateActor will validate this matches its expected sequence
        self.current_operation_id = Some(operation_id);

        // Extract port from handle (cheap Rc deref)
        #[cfg(target_arch = "wasm32")]
        let port = (*port_handle).clone();

        #[cfg(not(target_arch = "wasm32"))]
        let port = {
            // Native implementation would use a different port type
            return Err(ActorError::Transport(
                "Native port handling not implemented".to_string(),
            ));
        };

        // Parse framing to create SerialConfig
        let config = Self::parse_framing(&framing, baud).map_err(|e| {
            ActorError::Transport(format!("Failed to parse framing '{}': {}", framing, e))
        })?;

        // Create transport and open with retry logic
        let mut transport = WebSerialTransport::new();
        let mut last_error = None;

        for attempt in 1..=constants::port::MAX_OPEN_RETRIES {
            match transport.open(port.clone(), config.clone()).await {
                Ok(_) => {
                    actor_debug!(
                        "PortActor: Opened {} @ {} baud on attempt {}",
                        port_info.path,
                        baud,
                        attempt
                    );

                    // Wrap transport in Rc for sharing between actor and read loop
                    let transport_rc = std::rc::Rc::new(transport);
                    let sendable = SendableTransport(transport_rc.clone());

                    // Send wakeup if requested (triggers shell prompt)
                    if send_wakeup {
                        // Wait for device/UART to stabilize after open
                        #[cfg(target_arch = "wasm32")]
                        gloo_timers::future::sleep(std::time::Duration::from_millis(
                            constants::port::STABILIZATION_MS,
                        ))
                        .await;

                        // FIX: Send only CR (\r) to avoid double-newline issues with some shells
                        let _ = sendable.0.write(b"\r").await;
                    }

                    // Create shutdown channel for read loop
                    let (shutdown_tx, shutdown_rx) = mpsc::channel(100);

                    // Create done channel for cleanup coordination
                    let (done_tx, done_rx) = futures_channel::oneshot::channel();

                    // Spawn read loop
                    spawn_read_loop(
                        sendable.clone(),
                        self.event_tx.clone(),
                        self.state_tx.clone(),
                        shutdown_rx,
                        send_wakeup, // suppress echo if we sent wakeup
                        done_tx,
                    );

                    // Store transport, shutdown channel, and done receiver
                    self.transport = Some(sendable);
                    self.shutdown_tx = Some(shutdown_tx);
                    self.done_rx = Some(done_rx);
                    self.active_port = Some(port_info.path.clone());

                    // Notify StateActor - CRITICAL coordination message
                    // Must succeed, otherwise state machine becomes inconsistent
                    self.state_tx
                        .try_send(StateMessage::ConnectionEstablished { operation_id })
                        .map_err(|_| {
                            ActorError::ChannelClosed(
                                "StateActor unavailable during ConnectionEstablished".into(),
                            )
                        })?;

                    // Emit success event
                    let _ = self.event_tx.try_send(SystemEvent::StatusUpdate {
                        message: format!("Connected to {} @ {} baud", port_info.path, baud),
                    });

                    return Ok(());
                }
                Err(core_types::TransportError::AlreadyOpen)
                | Err(core_types::TransportError::InvalidState(_))
                    if attempt < constants::port::MAX_OPEN_RETRIES =>
                {
                    actor_debug!("PortActor: Open failed (attempt {}), retrying...", attempt);
                    // Calculate delay using shared backoff logic
                    let delay = crate::backoff::calculate_retry_delay(attempt);

                    #[cfg(target_arch = "wasm32")]
                    gloo_timers::future::sleep(std::time::Duration::from_millis(delay)).await;

                    // Capture the error but continue loop
                    last_error = Some(ActorError::Transport("Connection retry".into()));
                    continue;
                }
                Err(core_types::TransportError::ConnectionFailed(ref msg))
                    if attempt < constants::port::MAX_OPEN_RETRIES =>
                {
                    // Only retry specific retriable errors
                    // WebSerial API errors are opaque, must match on string content
                    let is_retriable = msg.contains("NetworkError")
                        || msg.contains("busy")
                        || msg.contains("in use")
                        || msg.contains("InvalidStateError"); // Port closing/reopening race

                    if is_retriable {
                        actor_debug!("PortActor: Retriable error (attempt {}): {}", attempt, msg);
                        let delay = crate::backoff::calculate_retry_delay(attempt);

                        #[cfg(target_arch = "wasm32")]
                        gloo_timers::future::sleep(std::time::Duration::from_millis(delay)).await;

                        last_error = Some(ActorError::Transport(msg.clone()));
                        continue;
                    } else {
                        // Fatal error - permission denied, invalid baud, etc.
                        actor_debug!("PortActor: Fatal error (not retriable): {}", msg);
                        last_error = Some(ActorError::Transport(msg.clone()));
                        break;
                    }
                }
                Err(e) => {
                    last_error = Some(ActorError::Transport(format!("{:?}", e)));
                    break;
                }
            }
        }

        // All retries failed
        let error_msg = if let Some(e) = last_error {
            format!("Failed to open port: {}", e)
        } else {
            "Max retries exceeded".to_string()
        };

        // CRITICAL coordination message - must succeed
        self.state_tx
            .try_send(StateMessage::ConnectionFailed {
                reason: error_msg.clone(),
            })
            .map_err(|_| {
                ActorError::ChannelClosed("StateActor unavailable during ConnectionFailed".into())
            })?;

        Err(ActorError::Transport(error_msg))
    }

    #[cfg(target_arch = "wasm32")]
    async fn handle_close(&mut self) -> Result<(), ActorError> {
        actor_debug!("PortActor: handle_close() called");

        // Send shutdown signal to read loop
        if let Some(mut shutdown_tx) = self.shutdown_tx.take() {
            let _ = shutdown_tx.try_send(());
            actor_debug!("PortActor: Shutdown signal sent to read loop");
        }

        // Drop our transport reference (allows read loop to unwrap and close)
        if let Some(_transport) = self.transport.take() {
            actor_debug!("PortActor: Dropped transport reference");
        }

        // Wait for read loop to complete cleanup with timeout
        // This ensures port is fully closed before sending ConnectionClosed
        if let Some(done_rx) = self.done_rx.take() {
            use futures::select;
            use futures::FutureExt;

            let mut timeout = gloo_timers::future::sleep(Duration::from_millis(
                constants::port::CLEANUP_TIMEOUT_MS,
            ))
            .fuse();
            let mut done = done_rx.fuse();

            select! {
                result = done => {
                    match result {
                        Ok(()) => {
                            actor_debug!("PortActor: Read loop cleanup confirmed");
                        }
                        Err(_) => {
                            actor_debug!("PortActor: Read loop done channel closed without signal");
                        }
                    }
                }
                _ = timeout => {
                    // Timeout - read loop may be stuck
                    actor_debug!("PortActor: Timeout waiting for read loop (500ms). Proceeding.");

                    // Check for potential resource leaks
                    if let Some(transport) = &self.transport {
                        let count = std::rc::Rc::strong_count(&transport.0);
                        if count > 1 {
                            #[cfg(target_arch = "wasm32")]
                            web_sys::console::warn_1(
                                &format!(
                                    "Port cleanup timeout - {} outstanding references. \
                                     Resources will be cleaned by Drop implementation.",
                                    count
                                ).into()
                            );
                        }
                    }
                }
            }
        } else {
            // Fallback: No done channel (old behavior for compatibility)
            actor_debug!("PortActor: No done channel, using fallback 100ms delay");
            gloo_timers::future::sleep(Duration::from_millis(100)).await;
        }

        self.active_port = None;
        self.current_operation_id = None;

        let _ = self.event_tx.try_send(SystemEvent::StatusUpdate {
            message: "Port closed".into(),
        });

        // Notify StateActor that close is complete (event-driven coordination)
        // CRITICAL coordination message - must succeed
        self.state_tx
            .try_send(StateMessage::ConnectionClosed)
            .map_err(|_| {
                ActorError::ChannelClosed("StateActor unavailable during ConnectionClosed".into())
            })?;

        actor_debug!("PortActor: Sent ConnectionClosed to StateActor");

        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    async fn handle_write(&mut self, data: Vec<u8>) -> Result<(), ActorError> {
        // If transport is None (port closed/not open), silently ignore write
        let transport = match self.transport.as_ref() {
            Some(t) => t,
            None => {
                actor_debug!("PortActor: Ignoring write - port not open");
                return Ok(());
            }
        };

        transport
            .write(&data)
            .await
            .map_err(|e| ActorError::Transport(format!("Write failed: {}", e)))?;

        // Emit TX activity indicator
        let _ = self.event_tx.try_send(SystemEvent::TxActivity);

        Ok(())
    }

    #[cfg(target_arch = "wasm32")]
    async fn handle_inject_data(&mut self, data: Vec<u8>) -> Result<(), ActorError> {
        let timestamp_us = (js_sys::Date::now() * 1000.0) as u64;

        let _ = self
            .event_tx
            .try_send(SystemEvent::DataReceived { data, timestamp_us });
        let _ = self.event_tx.try_send(SystemEvent::RxActivity);

        Ok(())
    }
}

#[cfg(target_arch = "wasm32")]
fn spawn_read_loop(
    transport: SendableTransport,
    mut event_tx: mpsc::Sender<SystemEvent>,
    mut state_tx: mpsc::Sender<StateMessage>,
    mut shutdown_rx: mpsc::Receiver<()>,
    suppress_echo: bool,
    done_tx: futures_channel::oneshot::Sender<()>,
) {
    // WebSerialTransport implements Send/Sync for WASM (single-threaded)
    // SendableTransport is safe to move into spawn_local (same thread)
    spawn_local(async move {
        let mut check_suppress = suppress_echo;

        loop {
            // Create futures for reading and shutdown
            let read_fut = transport.read_chunk().fuse();
            let shutdown_fut = shutdown_rx.next().fuse();

            futures::pin_mut!(read_fut, shutdown_fut);

            let read_result = futures::select! {
                res = read_fut => Some(res),
                _ = shutdown_fut => None, // Shutdown signal
            };

            match read_result {
                Some(Ok((mut data, timestamp_us))) if !data.is_empty() => {
                    if check_suppress {
                        // Strip leading whitespace (CR, LF) which are likely the echo of our wakeup
                        let start = data
                            .iter()
                            .position(|&b| b != b'\r' && b != b'\n' && b != 0)
                            .unwrap_or(data.len());
                        if start > 0 {
                            actor_debug!("PortActor: Suppressed {} echo bytes", start);
                            data = data.split_off(start);
                        }
                        // Only disable check if we actually found data or stripped something?
                        // Actually, if we got a packet, that's the response. Turn off check.
                        check_suppress = false;
                    }

                    if !data.is_empty() {
                        let _ = event_tx.try_send(SystemEvent::DataReceived { data, timestamp_us });
                        let _ = event_tx.try_send(SystemEvent::RxActivity);
                    }
                }
                Some(Err(_)) => {
                    // Connection lost
                    let _ = state_tx.try_send(StateMessage::ConnectionLost);
                    let _ = event_tx.try_send(SystemEvent::Error {
                        message: "Connection lost".to_string(),
                    });
                    break; // Exit read loop
                }
                None => {
                    // Shutdown signal received
                    break;
                }
                _ => {} // Empty read is OK (timeout)
            }
        }

        // CRITICAL: Close the port when exiting loop
        // Try to unwrap the Rc to get exclusive ownership for explicit close
        match std::rc::Rc::try_unwrap(transport.0) {
            Ok(mut t) => {
                let _ = t.close().await;
                actor_debug!("Read loop: Port closed (exclusive ownership)");
            }
            Err(rc) => {
                // Cannot close explicitly - multiple references still exist
                // The Drop implementation will handle cleanup when the last reference is dropped
                actor_debug!(
                    "Read loop: Cannot force close - Rc still shared (strong_count={}). \
                     Port will be cleaned by Drop implementation.",
                    std::rc::Rc::strong_count(&rc)
                );
                drop(rc);
            }
        }

        // Signal completion to PortActor (allows handle_close to wait for cleanup)
        let _ = done_tx.send(());
        actor_debug!("Read loop: Cleanup complete, signaled done");
    });
}

#[cfg(target_arch = "wasm32")]
impl Actor for PortActor {
    type Message = PortMessage;

    fn name(&self) -> &'static str {
        "PortActor"
    }

    async fn handle(&mut self, msg: PortMessage) -> Result<(), ActorError> {
        match msg {
            #[cfg(target_arch = "wasm32")]
            PortMessage::Open {
                port,
                baud,
                framing,
                send_wakeup,
                operation_id,
                port_handle,
            } => {
                self.handle_open(port, baud, framing, send_wakeup, operation_id, port_handle)
                    .await?
            }

            #[cfg(not(target_arch = "wasm32"))]
            PortMessage::Open {
                port,
                baud,
                framing,
                send_wakeup,
                operation_id,
            } => {
                self.handle_open(port, baud, framing, send_wakeup, operation_id)
                    .await?
            }

            PortMessage::Close => self.handle_close().await?,
            PortMessage::Write { data } => self.handle_write(data).await?,
            PortMessage::InjectData { data } => self.handle_inject_data(data).await?,
        }

        Ok(())
    }

    async fn shutdown(&mut self) {
        // Close port on shutdown
        let _ = self.handle_close().await;
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use super::*;
    use futures::stream::StreamExt;

    fn create_test_actor() -> (
        PortActor,
        mpsc::Receiver<StateMessage>,
        mpsc::Receiver<SystemEvent>,
    ) {
        let (state_tx, state_rx) = mpsc::channel(100);
        let (event_tx, event_rx) = mpsc::channel(100);

        let actor = PortActor::new(state_tx, event_tx);
        (actor, state_rx, event_rx)
    }

    #[tokio::test]
    async fn test_initial_state() {
        let (actor, _, _) = create_test_actor();
        assert!(actor.active_port.is_none());
    }

    #[tokio::test]
    async fn test_open_port_success() {
        let (mut actor, mut state_rx, mut event_rx) = create_test_actor();

        let port = actor_protocol::SerialPortInfo::new("/dev/ttyUSB0".into(), None, None);
        actor
            .handle_open(port.clone(), 115200, "8N1".into(), false)
            .await
            .unwrap();

        // Port should be marked as open
        assert_eq!(actor.active_port, Some("/dev/ttyUSB0".to_string()));

        // Should notify StateActor
        let state_msg = state_rx.next().await.unwrap();
        match state_msg {
            StateMessage::ConnectionEstablished => {}
            _ => panic!("Wrong message"),
        }

        // Should emit status event
        let event = event_rx.next().await.unwrap();
        match event {
            SystemEvent::StatusUpdate { message } => {
                assert!(message.contains("Connected"));
                assert!(message.contains("115200"));
            }
            _ => panic!("Wrong event"),
        }
    }

    #[tokio::test]
    async fn test_close_port() {
        let (mut actor, _, mut event_rx) = create_test_actor();

        // Simulate open port
        actor.active_port = Some("/dev/ttyUSB0".to_string());

        actor.handle_close().await.unwrap();

        // Port should be closed
        assert!(actor.active_port.is_none());

        // Should emit close event
        let event = event_rx.next().await.unwrap();
        match event {
            SystemEvent::StatusUpdate { message } => {
                assert_eq!(message, "Port closed");
            }
            _ => panic!("Wrong event"),
        }
    }

    #[tokio::test]
    async fn test_write_when_open() {
        let (mut actor, _, mut event_rx) = create_test_actor();

        actor.active_port = Some("/dev/ttyUSB0".to_string());

        actor.handle_write(vec![1, 2, 3]).await.unwrap();

        // Should emit TX activity
        let event = event_rx.next().await.unwrap();
        match event {
            SystemEvent::TxActivity => {}
            _ => panic!("Wrong event"),
        }
    }

    #[tokio::test]
    async fn test_write_when_closed_returns_ok() {
        // New error handling: Expected State pattern
        let (mut actor, _, _) = create_test_actor();

        // Write when closed should return Ok (not an error)
        let result = actor.handle_write(vec![1, 2, 3]).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_cannot_open_twice() {
        let (mut actor, _, _) = create_test_actor();

        actor.active_port = Some("/dev/ttyUSB0".to_string());

        let port = actor_protocol::SerialPortInfo::new("/dev/ttyUSB1".into(), None, None);
        let result = actor.handle_open(port, 115200, "8N1".into(), false).await;

        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_shutdown_closes_port() {
        let (mut actor, _, mut event_rx) = create_test_actor();

        actor.active_port = Some("/dev/ttyUSB0".to_string());

        actor.shutdown().await;

        assert!(actor.active_port.is_none());

        // Should emit close event
        let event = event_rx.next().await.unwrap();
        match event {
            SystemEvent::StatusUpdate { message } => {
                assert_eq!(message, "Port closed");
            }
            _ => panic!("Wrong event"),
        }
    }

    #[tokio::test]
    async fn test_close_sends_connection_closed_message() {
        let (mut actor, mut state_rx, _) = create_test_actor();

        actor.active_port = Some("/dev/ttyUSB0".to_string());

        actor.handle_close().await.unwrap();

        // Should send ConnectionClosed to StateActor
        let state_msg = state_rx.next().await.unwrap();
        match state_msg {
            StateMessage::ConnectionClosed => {}
            _ => panic!("Expected ConnectionClosed, got {:?}", state_msg),
        }
    }

    #[tokio::test]
    async fn test_close_when_already_closed_is_idempotent() {
        let (mut actor, _, _) = create_test_actor();

        // Close when already closed should succeed
        let result1 = actor.handle_close().await;
        assert!(result1.is_ok());

        let result2 = actor.handle_close().await;
        assert!(result2.is_ok());
    }

    #[tokio::test]
    async fn test_parse_framing_8n1() {
        let config = PortActor::parse_framing("8N1", 115200).unwrap();
        assert_eq!(config.baud_rate, 115200);
        assert_eq!(config.data_bits, 8);
        assert_eq!(config.parity, core_types::Parity::None);
        assert_eq!(config.stop_bits, core_types::StopBits::One);
    }

    #[tokio::test]
    async fn test_parse_framing_7e1() {
        let config = PortActor::parse_framing("7E1", 9600).unwrap();
        assert_eq!(config.baud_rate, 9600);
        assert_eq!(config.data_bits, 7);
        assert_eq!(config.parity, core_types::Parity::Even);
        assert_eq!(config.stop_bits, core_types::StopBits::One);
    }

    #[tokio::test]
    async fn test_parse_framing_8e1() {
        let config = PortActor::parse_framing("8E1", 57600).unwrap();
        assert_eq!(config.baud_rate, 57600);
        assert_eq!(config.data_bits, 8);
        assert_eq!(config.parity, core_types::Parity::Even);
        assert_eq!(config.stop_bits, core_types::StopBits::One);
    }

    #[tokio::test]
    async fn test_parse_framing_invalid_format() {
        let result = PortActor::parse_framing("INVALID", 19200);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("must be 3 characters"));
    }

    #[tokio::test]
    async fn test_parse_framing_invalid_data_bits() {
        let result = PortActor::parse_framing("5N1", 115200);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("must be 7 or 8"));
    }

    #[tokio::test]
    async fn test_parse_framing_invalid_parity() {
        let result = PortActor::parse_framing("8X1", 115200);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("must be N, E, or O"));
    }

    #[tokio::test]
    async fn test_parse_framing_invalid_stop_bits() {
        let result = PortActor::parse_framing("8N3", 115200);
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.contains("must be 1 or 2"));
    }

    #[tokio::test]
    async fn test_parse_framing_case_insensitive() {
        // Lowercase should work
        let config_lower = PortActor::parse_framing("8n1", 115200).unwrap();
        assert_eq!(config_lower.parity, core_types::Parity::None);

        // Mixed case
        let config_mixed = PortActor::parse_framing("7e1", 9600).unwrap();
        assert_eq!(config_mixed.parity, core_types::Parity::Even);

        let config_odd = PortActor::parse_framing("8o2", 57600).unwrap();
        assert_eq!(config_odd.parity, core_types::Parity::Odd);
    }

    #[test]
    fn test_backoff_calculation_increases() {
        // Test that backoff delay increases with attempts
        let delay1 = crate::backoff::calculate_retry_delay(1);
        let delay2 = crate::backoff::calculate_retry_delay(2);
        let delay3 = crate::backoff::calculate_retry_delay(3);

        assert!(delay2 > delay1);
        assert!(delay3 > delay2);
    }

    #[test]
    fn test_backoff_calculation_caps_at_max() {
        // Test that backoff delay is capped
        let delay_high = crate::backoff::calculate_retry_delay(50);
        let delay_higher = crate::backoff::calculate_retry_delay(100);

        // Should be capped, so they should be equal
        assert_eq!(delay_high, delay_higher);
    }
}
