/// Supervision utilities for actor operations
///
/// Provides timeout-based supervision to prevent actors from getting stuck
/// in long-running operations.
use crate::StateMessage;
use actor_protocol::ConnectionState;
use futures_channel::mpsc;
#[cfg(target_arch = "wasm32")]
use std::cell::Cell;
#[cfg(target_arch = "wasm32")]
use std::rc::Rc;

/// Handle to cancel a timeout operation
///
/// When dropped or explicitly cancelled, the timeout task will not send
/// the timeout message, preventing spurious timeouts after operations complete.
///
/// Platform-specific implementation:
/// - WASM (single-threaded): Uses Rc<Cell<bool>> for efficiency
/// - Native (multi-threaded tests): Uses Arc<AtomicBool> for thread-safety
#[derive(Clone)]
pub struct TimeoutHandle {
    #[cfg(target_arch = "wasm32")]
    cancelled: Rc<Cell<bool>>,
    #[cfg(not(target_arch = "wasm32"))]
    cancelled: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl TimeoutHandle {
    fn new() -> Self {
        Self {
            #[cfg(target_arch = "wasm32")]
            cancelled: Rc::new(Cell::new(false)),
            #[cfg(not(target_arch = "wasm32"))]
            cancelled: std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false)),
        }
    }

    /// Cancel the timeout, preventing it from firing
    pub fn cancel(&self) {
        #[cfg(target_arch = "wasm32")]
        self.cancelled.set(true);
        #[cfg(not(target_arch = "wasm32"))]
        self.cancelled
            .store(true, std::sync::atomic::Ordering::Release);
    }

    /// Check if this timeout has been cancelled (used internally by timeout task)
    #[allow(dead_code)]
    fn is_cancelled(&self) -> bool {
        #[cfg(target_arch = "wasm32")]
        {
            self.cancelled.get()
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            self.cancelled.load(std::sync::atomic::Ordering::Acquire)
        }
    }
}

impl Drop for TimeoutHandle {
    fn drop(&mut self) {
        // Auto-cancel when handle is dropped
        self.cancel();
    }
}

// Compile-time safety check for WASM: prevent using TimeoutHandle with atomics
// Note: This would require enabling the 'atomics' feature explicitly in Cargo.toml
#[cfg(all(target_arch = "wasm32", target_feature = "atomics"))]
compile_error!(
    "TimeoutHandle is unsafe with WASM atomics! \
     Rc<Cell<bool>> uses non-atomic operations which cause data races in multi-threaded WASM. \
     This feature combination is not supported."
);

/// Timeout configuration for supervised operations
pub struct SupervisionConfig {
    /// Timeout for probing operations (auto-baud detection)
    pub probe_timeout_secs: u64,
    /// Timeout for connection operations (port opening)
    pub connect_timeout_secs: u64,
    /// Timeout for auto-reconnection operations
    pub auto_reconnect_timeout_secs: u64,
    /// Timeout for disconnection operations (port closing)
    pub disconnect_timeout_secs: u64,
    /// Timeout for reconfiguration operations (changing baud rate)
    pub reconfigure_timeout_secs: u64,
}

impl Default for SupervisionConfig {
    fn default() -> Self {
        Self {
            probe_timeout_secs: 30,          // 30s for probing multiple baud rates
            connect_timeout_secs: 10,        // 10s for port opening
            auto_reconnect_timeout_secs: 10, // 10s for auto-reconnect
            disconnect_timeout_secs: 5,      // 5s for port closing
            reconfigure_timeout_secs: 10,    // 10s for reconfiguration
        }
    }
}

/// Spawn a timeout task that sends a timeout message after the specified duration
///
/// Returns a TimeoutHandle that can be used to cancel the timeout. If the handle
/// is dropped or explicitly cancelled before the timeout fires, no message will
/// be sent. This prevents spurious timeout messages after operations complete.
#[cfg(target_arch = "wasm32")]
pub fn spawn_timeout(
    state_tx: mpsc::Sender<StateMessage>,
    operation: &str,
    current_state: ConnectionState,
    timeout_secs: u64,
) -> TimeoutHandle {
    let operation = operation.to_string();
    let handle = TimeoutHandle::new();
    let cancel_flag = handle.cancelled.clone();

    wasm_bindgen_futures::spawn_local(async move {
        // Wait for timeout duration with periodic cancellation checks
        // This allows timeout tasks to exit early if cancelled, reducing resource usage
        let check_interval_ms = 500; // Check cancellation every 500ms
        let total_ms = timeout_secs * 1000;
        let mut elapsed_ms = 0;
        let mut state_tx = state_tx; // Make it mutable in async block

        while elapsed_ms < total_ms {
            // Check if cancelled (fast exit path)
            if cancel_flag.get() {
                return;
            }

            // Sleep for up to check_interval_ms
            let remaining_ms = total_ms - elapsed_ms;
            let sleep_ms = remaining_ms.min(check_interval_ms);
            gloo_timers::future::sleep(std::time::Duration::from_millis(sleep_ms)).await;
            elapsed_ms += sleep_ms;
        }

        // Final check before sending timeout message
        if !cancel_flag.get() {
            let _ = state_tx.try_send(StateMessage::OperationTimeout {
                operation,
                state: current_state,
            });
        }
    });

    handle
}

/// Native version for tests
/// In production, this is WASM-only, so native implementation is a test stub
#[cfg(all(not(target_arch = "wasm32"), test))]
pub fn spawn_timeout(
    state_tx: mpsc::Sender<StateMessage>,
    operation: &str,
    current_state: ConnectionState,
    timeout_secs: u64,
) -> TimeoutHandle {
    let operation = operation.to_string();
    let handle = TimeoutHandle::new();
    let cancel_flag = handle.cancelled.clone();

    tokio::spawn(async move {
        // Wait for timeout duration with periodic cancellation checks
        let check_interval_ms = 500;
        let total_ms = timeout_secs * 1000;
        let mut elapsed_ms = 0;
        let mut state_tx = state_tx; // Make it mutable in async block

        while elapsed_ms < total_ms {
            // Check if cancelled (fast exit path)
            if cancel_flag.load(std::sync::atomic::Ordering::Acquire) {
                return;
            }

            // Sleep for up to check_interval_ms
            let remaining_ms = total_ms - elapsed_ms;
            let sleep_ms = remaining_ms.min(check_interval_ms);
            tokio::time::sleep(tokio::time::Duration::from_millis(sleep_ms)).await;
            elapsed_ms += sleep_ms;
        }

        // Final check before sending timeout message
        if !cancel_flag.load(std::sync::atomic::Ordering::Acquire) {
            let _ = state_tx.try_send(StateMessage::OperationTimeout {
                operation,
                state: current_state,
            });
        }
    });

    handle
}

/// Native version for non-test builds (not used in production, stub only)
#[cfg(all(not(target_arch = "wasm32"), not(test)))]
pub fn spawn_timeout(
    _state_tx: mpsc::Sender<StateMessage>,
    _operation: &str,
    _current_state: ConnectionState,
    _timeout_secs: u64,
) -> TimeoutHandle {
    // No-op: Native production builds are not used (WASM-only application)
    // This stub exists only for compilation purposes
    TimeoutHandle::new()
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config() {
        let config = SupervisionConfig::default();
        assert_eq!(config.probe_timeout_secs, 30);
        assert_eq!(config.connect_timeout_secs, 10);
        assert_eq!(config.auto_reconnect_timeout_secs, 10);
        assert_eq!(config.disconnect_timeout_secs, 5);
        assert_eq!(config.reconfigure_timeout_secs, 10);
    }

    #[tokio::test]
    async fn test_timeout_fires() {
        use futures::stream::StreamExt;

        let (state_tx, mut state_rx) = mpsc::channel(100);

        // Keep handle alive so timeout can fire
        let _handle = spawn_timeout(
            state_tx,
            "test_operation",
            ConnectionState::Connecting,
            1, // 1 second for fast test
        );

        // Wait for timeout message
        let msg = state_rx.next().await.unwrap();
        match msg {
            StateMessage::OperationTimeout { operation, state } => {
                assert_eq!(operation, "test_operation");
                assert_eq!(state, ConnectionState::Connecting);
            }
            _ => panic!("Expected OperationTimeout"),
        }
    }

    #[tokio::test]
    async fn test_timeout_cancelled_on_drop() {
        use tokio::time::{sleep, Duration};

        let (state_tx, mut state_rx) = mpsc::channel(100);

        // Drop handle immediately to cancel timeout
        {
            let _handle = spawn_timeout(
                state_tx,
                "test_operation",
                ConnectionState::Connecting,
                1, // 1 second timeout
            );
            // Handle dropped here
        }

        // Wait longer than timeout duration
        sleep(Duration::from_millis(1500)).await;

        // Should not receive any message (timeout was cancelled)
        assert!(state_rx.try_next().is_ok_and(|msg| msg.is_none()));
    }
}
