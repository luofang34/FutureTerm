use core_types::{SerialConfig, SignalState, Transport, TransportError};
use js_sys::{ArrayBuffer, Uint8Array};
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{BinaryType, MessageEvent, WebSocket};

/// Timeout for WebSocket close operations (milliseconds)
///
/// **Value**: 1000ms (1 second)
///
/// **Rationale**: WebSocket.close() should complete quickly, but network
/// conditions or server issues may cause delays.
const WS_CLOSE_TIMEOUT_MS: i32 = 1000;

/// WebSocket Transport Implementation for Safari/Firefox bridge.
///
/// This transport connects to a local WebSocket server (bridge daemon)
/// that provides access to serial ports via the native SerialPort API.
///
/// **Architecture:**
/// ```text
/// Browser (Safari) -> WebSocket -> Bridge Daemon -> Native Serial Port
/// ```
///
/// **Binary Protocol:**
/// - Data frames are sent as binary WebSocket messages (ArrayBuffer)
/// - Control messages (open/close/config) use JSON-RPC style protocol
///
/// Note: This transport is WASM-only and requires `wasm32-unknown-unknown` target.
pub struct WebSocketTransport {
    ws: Option<WebSocket>,
    /// Received data buffer (shared between callback and read_chunk)
    rx_buffer: Rc<RefCell<Vec<u8>>>,
    /// Error state (shared between callback and main thread)
    error_state: Rc<RefCell<Option<String>>>,
    /// Closures must be kept alive for callbacks
    _on_message: Option<Closure<dyn FnMut(MessageEvent)>>,
    _on_error: Option<Closure<dyn FnMut(web_sys::ErrorEvent)>>,
    _on_close: Option<Closure<dyn FnMut(web_sys::CloseEvent)>>,
}

// SAFETY: WebSocketTransport is safe to Send/Sync ONLY in single-threaded WASM.
//
// WebSocketTransport holds JsValues which are !Send / !Sync by default because
// JavaScript objects are not thread-safe. However:
//
// 1. In single-threaded WASM (without atomics), there is no true parallelism.
//    All operations execute sequentially on the main thread via spawn_local.
// 2. The Transport trait requires Send + Sync to work with async actors.
// 3. If atomics feature is enabled (SharedArrayBuffer), this code MUST NOT compile
//    because true multi-threading would violate JS memory safety.
//
// This implementation is conditionally compiled to fail if atomics are enabled,
// preventing undefined behavior in multi-threaded WASM scenarios.

// Compile-time safety check: prevent WebSocketTransport with WASM atomics
#[cfg(feature = "atomics")]
compile_error!(
    "WebSocketTransport is unsafe with WASM atomics! \
     JsValue types are not thread-safe. Use a different transport for multi-threaded WASM."
);

// SAFETY: WebSocketTransport is always compiled for WASM target (due to wasm-bindgen dependency).
// This crate has no meaning on non-WASM platforms. The Send/Sync impls are safe because:
// 1. WASM is single-threaded by default (without atomics feature)
// 2. The atomics feature is explicitly checked above with compile_error!
// 3. All async operations run on the main thread via spawn_local
#[cfg(not(feature = "atomics"))]
unsafe impl Send for WebSocketTransport {}

#[cfg(not(feature = "atomics"))]
unsafe impl Sync for WebSocketTransport {}

impl WebSocketTransport {
    /// Create a new WebSocket transport (not yet connected).
    pub fn new() -> Self {
        Self {
            ws: None,
            rx_buffer: Rc::new(RefCell::new(Vec::new())),
            error_state: Rc::new(RefCell::new(None)),
            _on_message: None,
            _on_error: None,
            _on_close: None,
        }
    }

    /// Connect to the WebSocket bridge server.
    ///
    /// **Parameters:**
    /// - `url`: WebSocket URL (e.g., "wss://127.0.0.1:9876")
    ///
    /// **Returns:**
    /// - `Ok(())` if connection initiated successfully
    /// - `Err(TransportError)` if WebSocket creation fails
    ///
    /// **Note:** This method returns immediately. The connection is established
    /// asynchronously. Check connection state via `is_open()` or wait for
    /// onopen event.
    pub async fn connect(&mut self, url: &str) -> Result<(), TransportError> {
        #[cfg(debug_assertions)]
        web_sys::console::log_1(&format!("WebSocketTransport: connecting to {}", url).into());

        // Create WebSocket
        let ws = WebSocket::new(url).map_err(|e| {
            TransportError::ConnectionFailed(format!("Failed to create WebSocket: {:?}", e))
        })?;

        // Set binary type to arraybuffer (not blob)
        ws.set_binary_type(BinaryType::Arraybuffer);

        // Setup callbacks
        let rx_buffer = self.rx_buffer.clone();
        let error_state = self.error_state.clone();

        // onmessage: Receive binary data
        let on_message = Closure::wrap(Box::new(move |event: MessageEvent| {
            if let Ok(data) = event.data().dyn_into::<ArrayBuffer>() {
                let array = Uint8Array::new(&data);
                let bytes = array.to_vec();

                #[cfg(debug_assertions)]
                web_sys::console::log_1(
                    &format!("WebSocketTransport: received {} bytes", bytes.len()).into(),
                );

                rx_buffer.borrow_mut().extend_from_slice(&bytes);
            }
        }) as Box<dyn FnMut(MessageEvent)>);

        ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

        // onerror: Log errors
        let error_state_err = error_state.clone();
        let on_error = Closure::wrap(Box::new(move |event: web_sys::ErrorEvent| {
            let msg = format!("WebSocket error: {}", event.message());
            #[cfg(debug_assertions)]
            web_sys::console::error_1(&msg.clone().into());

            *error_state_err.borrow_mut() = Some(msg);
        }) as Box<dyn FnMut(web_sys::ErrorEvent)>);

        ws.set_onerror(Some(on_error.as_ref().unchecked_ref()));

        // onclose: Log closure
        let on_close = Closure::wrap(Box::new(move |event: web_sys::CloseEvent| {
            #[cfg(debug_assertions)]
            web_sys::console::log_1(
                &format!(
                    "WebSocket closed: code={}, reason={}",
                    event.code(),
                    event.reason()
                )
                .into(),
            );
        }) as Box<dyn FnMut(web_sys::CloseEvent)>);

        ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));

        // Wait for connection to open
        let open_promise = js_sys::Promise::new(&mut |resolve, reject| {
            let ws_clone = ws.clone();
            let resolve_clone = resolve.clone();
            let reject_clone = reject.clone();

            let onopen = Closure::once(move || {
                let _ = resolve_clone.call0(&JsValue::NULL);
            });

            let onerror_once = Closure::once(move |_: web_sys::ErrorEvent| {
                let _ = reject_clone.call1(&JsValue::NULL, &"Connection failed".into());
            });

            ws_clone.set_onopen(Some(onopen.as_ref().unchecked_ref()));
            ws_clone.set_onerror(Some(onerror_once.as_ref().unchecked_ref()));

            onopen.forget();
            onerror_once.forget();
        });

        JsFuture::from(open_promise)
            .await
            .map_err(|e| TransportError::ConnectionFailed(format!("Connection failed: {:?}", e)))?;

        #[cfg(debug_assertions)]
        web_sys::console::log_1(&"WebSocketTransport: connected".into());

        // Store state
        self.ws = Some(ws);
        self._on_message = Some(on_message);
        self._on_error = Some(on_error);
        self._on_close = Some(on_close);

        Ok(())
    }

    /// Open a serial port via the bridge daemon.
    ///
    /// This is a bridge-specific method that sends a JSON-RPC message to the
    /// daemon requesting to open a serial port.
    ///
    /// **Note:** The bridge daemon must be running and connected via WebSocket
    /// before calling this method.
    pub async fn open_serial(
        &self,
        port_path: &str,
        config: SerialConfig,
    ) -> Result<(), TransportError> {
        let ws = self.ws.as_ref().ok_or(TransportError::NotConnected)?;

        // Send JSON-RPC message to bridge daemon
        let request = serde_json::json!({
            "type": "open",
            "port": port_path,
            "config": {
                "baud_rate": config.baud_rate,
                "data_bits": config.data_bits,
                "stop_bits": config.stop_bits,
                "parity": config.parity.as_str(),
                "flow_control": config.flow_control.as_str(),
            }
        });

        let msg = serde_json::to_string(&request).map_err(|e| {
            TransportError::Io(format!("Failed to serialize open request: {:?}", e))
        })?;

        ws.send_with_str(&msg)
            .map_err(|e| TransportError::Io(format!("Failed to send open request: {:?}", e)))?;

        #[cfg(debug_assertions)]
        web_sys::console::log_1(
            &format!("WebSocketTransport: sent open request for {}", port_path).into(),
        );

        Ok(())
    }
}

impl Default for WebSocketTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for WebSocketTransport {
    fn drop(&mut self) {
        // CRITICAL: Ensure resources are released even if close() wasn't called explicitly
        if self.ws.is_none() {
            return;
        }

        // Warn if dropping while open (indicates potential bug where close() wasn't called)
        #[cfg(debug_assertions)]
        {
            if self.ws.is_some() {
                web_sys::console::warn_1(
                    &"WebSocketTransport dropped while open - attempting cleanup".into(),
                );
            }
        }

        // Close WebSocket synchronously (fire and forget)
        if let Some(ws) = self.ws.take() {
            let _ = ws.close();
        }

        #[cfg(debug_assertions)]
        web_sys::console::log_1(&"WebSocketTransport: Drop cleanup complete".into());
    }
}

// Implement the shared Transport trait
impl Transport for WebSocketTransport {
    fn is_open(&self) -> bool {
        self.ws
            .as_ref()
            .map(|ws| ws.ready_state() == WebSocket::OPEN)
            .unwrap_or(false)
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        #[cfg(debug_assertions)]
        let start_close = js_sys::Date::now();

        if let Some(ws) = self.ws.take() {
            // Send close frame
            ws.close()
                .map_err(|e| TransportError::Io(format!("Failed to close WebSocket: {:?}", e)))?;

            // Wait for close to complete (with timeout)
            let close_promise = js_sys::Promise::new(&mut |resolve, _reject| {
                let ws_clone = ws.clone();
                let resolve_clone = resolve.clone();

                let onclose = Closure::once(move || {
                    let _ = resolve_clone.call0(&JsValue::NULL);
                });

                ws_clone.set_onclose(Some(onclose.as_ref().unchecked_ref()));
                onclose.forget();
            });

            let timeout_promise = js_sys::Promise::new(&mut |resolve, _reject| {
                if let Some(window) = web_sys::window() {
                    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                        &resolve,
                        WS_CLOSE_TIMEOUT_MS,
                    );
                }
            });

            let race_result =
                js_sys::Promise::race(&js_sys::Array::of2(&close_promise, &timeout_promise));
            let _ = JsFuture::from(race_result).await;
        }

        // Clear callbacks
        self._on_message = None;
        self._on_error = None;
        self._on_close = None;

        #[cfg(debug_assertions)]
        {
            let total_close = js_sys::Date::now() - start_close;
            web_sys::console::log_1(
                &format!(
                    "WebSocketTransport: close() complete in {:.1}ms",
                    total_close
                )
                .into(),
            );
        }
        Ok(())
    }

    async fn read_chunk(&self) -> Result<(Vec<u8>, u64), TransportError> {
        // Check for errors
        if let Some(err) = self.error_state.borrow().as_ref() {
            return Err(TransportError::Io(err.clone()));
        }

        // Check if WebSocket is still open
        if !self.is_open() {
            return Err(TransportError::Io("WebSocket closed".into()));
        }

        // Check if data is available
        let mut buffer = self.rx_buffer.borrow_mut();
        if buffer.is_empty() {
            // No data available, return empty (non-blocking read)
            return Ok((Vec::new(), 0));
        }

        // Drain buffer and return data
        let data = buffer.drain(..).collect();

        // Get timestamp (worker compatible)
        let global = js_sys::global();
        let perf_val =
            js_sys::Reflect::get(&global, &"performance".into()).unwrap_or(JsValue::UNDEFINED);

        let ts_ms = if !perf_val.is_undefined() {
            let perf: web_sys::Performance = perf_val.unchecked_into();
            perf.now()
        } else {
            js_sys::Date::now()
        };

        Ok((data, (ts_ms * 1000.0) as u64))
    }

    async fn write(&self, data: &[u8]) -> Result<(), TransportError> {
        let ws = self.ws.as_ref().ok_or(TransportError::NotConnected)?;

        // Send as binary WebSocket frame (avoid base64 overhead)
        ws.send_with_u8_array(data)
            .map_err(|e| TransportError::Io(format!("Failed to send data: {:?}", e)))?;

        Ok(())
    }

    async fn set_signals(&self, dtr: bool, rts: bool) -> Result<(), TransportError> {
        let ws = self.ws.as_ref().ok_or(TransportError::NotConnected)?;

        // Send JSON-RPC message to bridge daemon
        let request = serde_json::json!({
            "type": "set_signals",
            "dtr": dtr,
            "rts": rts,
        });

        let msg = serde_json::to_string(&request).map_err(|e| {
            TransportError::Io(format!("Failed to serialize set_signals request: {:?}", e))
        })?;

        ws.send_with_str(&msg).map_err(|e| {
            TransportError::Io(format!("Failed to send set_signals request: {:?}", e))
        })?;

        Ok(())
    }

    async fn get_signals(&self) -> Result<SignalState, TransportError> {
        let ws = self.ws.as_ref().ok_or(TransportError::NotConnected)?;

        // Send JSON-RPC message to bridge daemon
        let request = serde_json::json!({
            "type": "get_signals",
        });

        let msg = serde_json::to_string(&request).map_err(|e| {
            TransportError::Io(format!("Failed to serialize get_signals request: {:?}", e))
        })?;

        ws.send_with_str(&msg).map_err(|e| {
            TransportError::Io(format!("Failed to send get_signals request: {:?}", e))
        })?;

        // Wait for response (simplified - in production would need request/response matching)
        // For now, return default state as this is not critical for Phase 1
        Ok(SignalState {
            dtr: false,
            rts: false,
            dcd: false,
            dsr: false,
            ri: false,
            cts: false,
        })
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod wasm_tests {
    use super::*;
    use wasm_bindgen_test::*;

    wasm_bindgen_test_configure!(run_in_browser);

    #[wasm_bindgen_test]
    async fn test_websocket_creation() {
        let mut transport = WebSocketTransport::new();
        assert!(!transport.is_open());

        // Test connection to invalid URL (should fail gracefully)
        let result = transport.connect("ws://localhost:99999").await;
        assert!(result.is_err());
    }

    #[wasm_bindgen_test]
    fn test_transport_default() {
        let transport = WebSocketTransport::default();
        assert!(!transport.is_open());
    }
}
