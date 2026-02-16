use core_types::{SignalState, Transport, TransportError};
use js_sys::{ArrayBuffer, Uint8Array};
use serde::Deserialize;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{BinaryType, MessageEvent, WebSocket};

/// Timeout for WebSocket close operations (milliseconds)
const WS_CLOSE_TIMEOUT_MS: i32 = 1000;

/// Port info returned by bridge daemon's list_ports command
#[derive(Debug, Clone, Deserialize)]
pub struct BridgePortInfo {
    pub path: String,
    #[serde(default)]
    pub port_type: String,
    pub vid: Option<u16>,
    pub pid: Option<u16>,
    pub serial_number: Option<String>,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
}

/// WebSocket Transport for Safari/Firefox bridge daemon.
///
/// Communicates with the local bridge daemon using JSON-RPC style messages.
/// Serial data is base64-encoded in JSON text frames.
///
/// **Architecture:**
/// ```text
/// Browser (Safari) -> WebSocket (JSON) -> Bridge Daemon -> Native Serial Port
/// ```
pub struct WebSocketTransport {
    ws: Option<WebSocket>,
    /// Received serial data buffer (decoded from base64 JSON messages)
    rx_buffer: Rc<RefCell<Vec<u8>>>,
    /// Control message buffer (JSON responses: ports_list, opened, error, etc.)
    control_messages: Rc<RefCell<Vec<String>>>,
    /// Error state (shared between callback and main thread)
    error_state: Rc<RefCell<Option<String>>>,
    /// Message ID counter for request-response matching
    next_id: Rc<Cell<u64>>,
    /// Closures must be kept alive for callbacks
    _on_message: Option<Closure<dyn FnMut(MessageEvent)>>,
    _on_error: Option<Closure<dyn FnMut(web_sys::ErrorEvent)>>,
    _on_close: Option<Closure<dyn FnMut(web_sys::CloseEvent)>>,
}

// SAFETY: WebSocketTransport is safe to Send/Sync ONLY in single-threaded WASM.
// See original safety comments - JsValues are !Send/!Sync by default but
// single-threaded WASM (without atomics) has no true parallelism.
#[cfg(feature = "atomics")]
compile_error!(
    "WebSocketTransport is unsafe with WASM atomics! \
     JsValue types are not thread-safe. Use a different transport for multi-threaded WASM."
);

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
            control_messages: Rc::new(RefCell::new(Vec::new())),
            error_state: Rc::new(RefCell::new(None)),
            next_id: Rc::new(Cell::new(1)),
            _on_message: None,
            _on_error: None,
            _on_close: None,
        }
    }

    /// Get the next message ID for request-response matching
    fn next_msg_id(&self) -> u64 {
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        id
    }

    /// Connect to the WebSocket bridge server.
    pub async fn connect(&mut self, url: &str) -> Result<(), TransportError> {
        #[cfg(debug_assertions)]
        web_sys::console::log_1(&format!("WebSocketTransport: connecting to {}", url).into());

        let ws = WebSocket::new(url).map_err(|e| {
            TransportError::ConnectionFailed(format!("Failed to create WebSocket: {:?}", e))
        })?;

        ws.set_binary_type(BinaryType::Arraybuffer);

        // Setup callbacks
        let rx_buffer = self.rx_buffer.clone();
        let control_messages = self.control_messages.clone();
        let error_state = self.error_state.clone();

        // onmessage: Handle both JSON text (daemon protocol) and binary data
        let on_message = Closure::wrap(Box::new(move |event: MessageEvent| {
            let data = event.data();

            // Handle text messages (JSON from bridge daemon)
            if let Some(text) = data.as_string() {
                let parsed = match serde_json::from_str::<serde_json::Value>(&text) {
                    Ok(v) => v,
                    Err(_) => return,
                };

                let msg_type = parsed.get("type").and_then(|t| t.as_str()).unwrap_or("");

                if msg_type == "data" {
                    // Serial data from device - decode base64 to bytes
                    if let Some(b64_data) = parsed.get("data").and_then(|d| d.as_str()) {
                        match base64_decode(b64_data) {
                            Ok(bytes) => {
                                #[cfg(debug_assertions)]
                                web_sys::console::log_1(
                                    &format!("Bridge RX: {} bytes", bytes.len()).into(),
                                );
                                rx_buffer.borrow_mut().extend_from_slice(&bytes);
                            }
                            Err(_e) => {
                                #[cfg(debug_assertions)]
                                web_sys::console::error_1(
                                    &format!("Bridge: base64 decode error: {}", _e).into(),
                                );
                            }
                        }
                    }
                } else {
                    // Control message (ports_list, opened, closed, error, etc.)
                    #[cfg(debug_assertions)]
                    web_sys::console::log_1(&format!("Bridge control: type={}", msg_type).into());
                    control_messages.borrow_mut().push(text);
                }
                return;
            }

            // Handle binary data (ArrayBuffer) - for forward compatibility
            if let Ok(array_buffer) = data.dyn_into::<ArrayBuffer>() {
                let array = Uint8Array::new(&array_buffer);
                let bytes = array.to_vec();
                rx_buffer.borrow_mut().extend_from_slice(&bytes);
            }
        }) as Box<dyn FnMut(MessageEvent)>);

        ws.set_onmessage(Some(on_message.as_ref().unchecked_ref()));

        // onerror: Track errors
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

        // Restore proper error/close handlers (open_promise temporarily overwrites onerror)
        ws.set_onerror(Some(on_error.as_ref().unchecked_ref()));
        ws.set_onclose(Some(on_close.as_ref().unchecked_ref()));

        #[cfg(debug_assertions)]
        web_sys::console::log_1(&"WebSocketTransport: connected".into());

        self.ws = Some(ws);
        self._on_message = Some(on_message);
        self._on_error = Some(on_error);
        self._on_close = Some(on_close);

        Ok(())
    }

    /// Clear the receive buffer (used during baud rate probing to discard stale data)
    pub fn clear_rx_buffer(&self) {
        self.rx_buffer.borrow_mut().clear();
    }

    // ═══════════════════════════════════════════════════════════════
    // Bridge Daemon Protocol Methods
    // ═══════════════════════════════════════════════════════════════

    /// Send a JSON string to the bridge daemon
    fn send_json(&self, msg: &str) -> Result<(), TransportError> {
        let ws = self.ws.as_ref().ok_or(TransportError::NotConnected)?;
        ws.send_with_str(msg)
            .map_err(|e| TransportError::Io(format!("Failed to send: {:?}", e)))?;
        Ok(())
    }

    /// Wait for a control response matching the expected type and ID
    async fn wait_for_response(
        &self,
        expected_type: &str,
        expected_id: u64,
    ) -> Result<serde_json::Value, TransportError> {
        // 50 iterations * 100ms = 5 second timeout
        for _ in 0..50 {
            {
                let mut messages = self.control_messages.borrow_mut();

                let mut found_idx = None;
                let mut error_result = None;

                for (i, msg) in messages.iter().enumerate() {
                    if let Ok(parsed) = serde_json::from_str::<serde_json::Value>(msg) {
                        let msg_type = parsed.get("type").and_then(|t| t.as_str()).unwrap_or("");
                        let msg_id = parsed.get("id").and_then(|id| id.as_u64());

                        if msg_type == expected_type && msg_id == Some(expected_id) {
                            found_idx = Some((i, parsed));
                            break;
                        }

                        if msg_type == "error" && msg_id == Some(expected_id) {
                            let error_msg = parsed
                                .get("message")
                                .and_then(|m| m.as_str())
                                .unwrap_or("Unknown error")
                                .to_string();
                            error_result = Some((i, error_msg));
                            break;
                        }
                    }
                }

                if let Some((idx, parsed)) = found_idx {
                    messages.remove(idx);
                    return Ok(parsed);
                }

                if let Some((idx, error_msg)) = error_result {
                    messages.remove(idx);
                    return Err(TransportError::Io(error_msg));
                }
            } // drop borrow before await

            sleep_ms(100).await;
        }

        Err(TransportError::Io("Timeout waiting for response".into()))
    }

    /// List available serial ports via bridge daemon
    pub async fn list_ports(&self) -> Result<Vec<BridgePortInfo>, TransportError> {
        let id = self.next_msg_id();
        let request = format!(r#"{{"type":"list_ports","id":{}}}"#, id);
        self.send_json(&request)?;

        let response = self.wait_for_response("ports_list", id).await?;

        let ports = response
            .get("ports")
            .ok_or_else(|| TransportError::Io("No ports in response".into()))?;

        serde_json::from_value(ports.clone())
            .map_err(|e| TransportError::Io(format!("Failed to parse ports: {}", e)))
    }

    /// Open a serial port via bridge daemon
    pub async fn open_port(&self, path: &str, baud_rate: u32) -> Result<(), TransportError> {
        let id = self.next_msg_id();
        let request = serde_json::json!({
            "type": "open",
            "id": id,
            "path": path,
            "baud_rate": baud_rate
        });
        self.send_json(&request.to_string())?;

        let _ = self.wait_for_response("opened", id).await?;
        Ok(())
    }

    /// Close the serial port via bridge daemon
    pub async fn close_port(&self) -> Result<(), TransportError> {
        let id = self.next_msg_id();
        let request = format!(r#"{{"type":"close","id":{}}}"#, id);
        self.send_json(&request)?;

        let _ = self.wait_for_response("closed", id).await?;
        Ok(())
    }

    /// Set serial config via bridge daemon
    pub async fn set_baud_rate(&self, baud_rate: u32) -> Result<(), TransportError> {
        let id = self.next_msg_id();
        let request = serde_json::json!({
            "type": "set_config",
            "id": id,
            "baud_rate": baud_rate
        });
        self.send_json(&request.to_string())?;

        let _ = self.wait_for_response("config_set", id).await?;
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
        if let Some(ws) = self.ws.take() {
            #[cfg(debug_assertions)]
            web_sys::console::log_1(&"WebSocketTransport: dropping, closing WebSocket".into());
            let _ = ws.close();
        }
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
        if let Some(ws) = self.ws.take() {
            ws.close()
                .map_err(|e| TransportError::Io(format!("Failed to close WebSocket: {:?}", e)))?;

            // Wait for close with timeout
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

        self._on_message = None;
        self._on_error = None;
        self._on_close = None;

        #[cfg(debug_assertions)]
        web_sys::console::log_1(&"WebSocketTransport: closed".into());

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

        // Drain buffer
        let mut buffer = self.rx_buffer.borrow_mut();
        if buffer.is_empty() {
            return Ok((Vec::new(), 0));
        }

        let data = buffer.drain(..).collect();

        // Get timestamp
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

        // Encode data as base64 and send via bridge JSON protocol
        let id = self.next_msg_id();
        let encoded = base64_encode(data);
        let request = serde_json::json!({
            "type": "write",
            "id": id,
            "data": encoded
        });

        ws.send_with_str(&request.to_string())
            .map_err(|e| TransportError::Io(format!("Failed to send write: {:?}", e)))?;

        // Fire-and-forget for low latency (don't wait for "written" response)
        Ok(())
    }

    async fn set_signals(&self, _dtr: bool, _rts: bool) -> Result<(), TransportError> {
        // Bridge daemon doesn't support signal control yet
        Ok(())
    }

    async fn get_signals(&self) -> Result<SignalState, TransportError> {
        // Bridge daemon doesn't support signal readback yet
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

// ═══════════════════════════════════════════════════════════════
// Helper Functions
// ═══════════════════════════════════════════════════════════════

fn base64_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}

fn base64_decode(data: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| format!("Base64 decode error: {}", e))
}

/// Sleep for the given number of milliseconds (WASM-compatible)
async fn sleep_ms(ms: i32) {
    let promise = js_sys::Promise::new(&mut |resolve, _reject| {
        if let Some(window) = web_sys::window() {
            let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, ms);
        }
    });
    let _ = JsFuture::from(promise).await;
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

    #[wasm_bindgen_test]
    fn test_base64_roundtrip() {
        let data = b"Hello, Bridge!";
        let encoded = base64_encode(data);
        let decoded = base64_decode(&encoded).unwrap_or_default();
        assert_eq!(decoded, data);
    }
}
