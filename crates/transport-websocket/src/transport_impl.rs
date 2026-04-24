use core_types::{SignalState, Transport, TransportError};
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::WebSocket;

use super::{base64_encode, WebSocketTransport, WS_CLOSE_TIMEOUT_MS};

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
