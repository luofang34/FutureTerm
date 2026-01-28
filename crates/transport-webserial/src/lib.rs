use core_types::{SerialConfig, SignalState, Transport, TransportError};
use js_sys::Uint8Array;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{
    ReadableStreamDefaultReader, SerialOptions, SerialPort, WritableStreamDefaultWriter,
};

/// Timeout for Drop cleanup operations (milliseconds)
///
/// **Value**: 100ms
///
/// **Rationale**: Drop implementation spawns async cleanup task to release
/// WebSerial API resources. This timeout prevents hanging indefinitely if:
/// - Device was unplugged (close() promise never resolves)
/// - Browser is shutting down (async tasks may be cancelled)
///
/// At 100ms: Fast enough to not block shutdown, long enough for clean disconnect.
#[cfg(target_arch = "wasm32")]
const DROP_CLEANUP_TIMEOUT_MS: i32 = 100;

/// Timeout for writer.close() operations (milliseconds)
///
/// **Value**: 200ms
///
/// **Rationale**: WritableStream.close() can hang if device disconnected.
/// Based on WebSerial API behavior:
/// - Normal close: 10-50ms
/// - Device disconnected: Hangs indefinitely
///
/// At 200ms: Allows clean shutdown while preventing reconnection delays.
const WRITER_CLOSE_TIMEOUT_MS: i32 = 200;

/// Timeout for port.close() operations (milliseconds)
///
/// **Value**: 600ms
///
/// **Rationale**: SerialPort.close() is slowest WebSerial operation:
/// - Writer close: 200ms
/// - Reader cancel: 50ms
/// - Port lock release: 200ms
/// - USB controller cleanup: 100-150ms
///
/// Total worst-case: ~600ms. This matches transport-webserial design goal
/// of preventing reconnection delays beyond 1 second.
///
/// **Trade-off**: Longer timeout ensures clean close, shorter prevents UI hangs.
const PORT_CLOSE_TIMEOUT_MS: i32 = 600;

/// WebSerial Transport Implementation.
///
/// Note: Usage requires RUSTFLAGS="--cfg=web_sys_unstable_apis"
pub struct WebSerialTransport {
    port: Option<SerialPort>,
    reader: Option<ReadableStreamDefaultReader>,
    writer: Option<WritableStreamDefaultWriter>,
    pending_read: std::cell::RefCell<Option<js_sys::Promise>>,
}

// SAFETY: WebSerialTransport is safe to Send/Sync ONLY in single-threaded WASM.
//
// WebSerialTransport holds JsValues which are !Send / !Sync by default because
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

// Compile-time safety check: prevent WebSerialTransport with WASM atomics
#[cfg(feature = "atomics")]
compile_error!(
    "WebSerialTransport is unsafe with WASM atomics! \
     JsValue types are not thread-safe. Use a different transport for multi-threaded WASM."
);

// SAFETY: WebSerialTransport is always compiled for WASM target (due to wasm-bindgen dependency).
// This crate has no meaning on non-WASM platforms. The Send/Sync impls are safe because:
// 1. WASM is single-threaded by default (without atomics feature)
// 2. The atomics feature is explicitly checked above with compile_error!
// 3. All async operations run on the main thread via spawn_local
#[cfg(not(feature = "atomics"))]
unsafe impl Send for WebSerialTransport {}

#[cfg(not(feature = "atomics"))]
unsafe impl Sync for WebSerialTransport {}

impl WebSerialTransport {
    pub fn new() -> Self {
        Self {
            port: None,
            reader: None,
            writer: None,
            pending_read: std::cell::RefCell::new(None),
        }
    }
}

impl Default for WebSerialTransport {
    fn default() -> Self {
        Self::new()
    }
}

impl Drop for WebSerialTransport {
    fn drop(&mut self) {
        // CRITICAL: Ensure resources are released even if close() wasn't called explicitly
        //
        // WebSerial API requires async close(), but Drop is sync. This is a best-effort
        // cleanup that spawns a detached async task to release streams and port.
        //
        // If the transport is already closed (port is None), skip cleanup.
        if self.port.is_none() && self.reader.is_none() && self.writer.is_none() {
            return;
        }

        // Warn if dropping while open (indicates potential bug where close() wasn't called)
        #[cfg(debug_assertions)]
        {
            if self.port.is_some() || self.reader.is_some() || self.writer.is_some() {
                #[cfg(target_arch = "wasm32")]
                web_sys::console::warn_1(
                    &"WebSerialTransport dropped while open - attempting cleanup".into(),
                );
            }
        }

        // Spawn detached async cleanup task
        let _reader = self.reader.take();
        let _writer = self.writer.take();
        let _port = self.port.take();

        #[cfg(target_arch = "wasm32")]
        wasm_bindgen_futures::spawn_local(async move {
            // Cancel and release reader
            if let Some(r) = _reader {
                let _ = JsFuture::from(r.cancel()).await;
                r.release_lock();
            }

            // Close and release writer (with timeout to avoid hanging)
            if let Some(w) = _writer {
                let close_promise = w.close();
                let timeout_promise = js_sys::Promise::new(&mut |resolve, _reject| {
                    if let Some(window) = web_sys::window() {
                        let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                            &resolve,
                            DROP_CLEANUP_TIMEOUT_MS,
                        );
                    }
                });

                let race_result =
                    js_sys::Promise::race(&js_sys::Array::of2(&close_promise, &timeout_promise));
                let _ = JsFuture::from(race_result).await;

                w.release_lock();
            }

            // Close port (with timeout)
            if let Some(p) = _port {
                if let Ok(func_val) = js_sys::Reflect::get(&p, &"close".into()) {
                    if let Ok(func) = func_val.dyn_into::<js_sys::Function>() {
                        if let Ok(promise_val) = func.call0(&p) {
                            let close_promise = js_sys::Promise::from(promise_val);
                            let timeout_promise = js_sys::Promise::new(&mut |resolve, _reject| {
                                if let Some(window) = web_sys::window() {
                                    let _ = window
                                        .set_timeout_with_callback_and_timeout_and_arguments_0(
                                            &resolve,
                                            DROP_CLEANUP_TIMEOUT_MS,
                                        );
                                }
                            });

                            let race_result = js_sys::Promise::race(&js_sys::Array::of2(
                                &close_promise,
                                &timeout_promise,
                            ));
                            let _ = JsFuture::from(race_result).await;
                        }
                    }
                }
            }

            #[cfg(debug_assertions)]
            web_sys::console::log_1(&"WebSerialTransport: Drop cleanup complete".into());
        });
    }
}

impl WebSerialTransport {
    /// Open the port with specified configuration.
    // Note: Mutability is required here to set self.port, self.reader, self.writer
    pub async fn open(
        &mut self,
        port: SerialPort,
        config: SerialConfig,
    ) -> Result<(), TransportError> {
        #[cfg(debug_assertions)]
        web_sys::console::log_1(
            &format!(
                "WebSerialTransport: open() called. Baud: {}",
                config.baud_rate
            )
            .into(),
        );

        let options = js_sys::Object::new();
        let _ = js_sys::Reflect::set(
            &options,
            &"baudRate".into(),
            &JsValue::from(config.baud_rate),
        );
        let _ = js_sys::Reflect::set(
            &options,
            &"dataBits".into(),
            &JsValue::from(config.data_bits),
        );
        let _ = js_sys::Reflect::set(
            &options,
            &"stopBits".into(),
            &JsValue::from(config.stop_bits),
        );
        let _ = js_sys::Reflect::set(
            &options,
            &"parity".into(),
            &JsValue::from(config.parity.as_str()),
        );
        let _ = js_sys::Reflect::set(
            &options,
            &"flowControl".into(),
            &JsValue::from(config.flow_control.as_str()),
        );

        // Convert to SerialOptions
        let serial_options: SerialOptions = options.unchecked_into();

        #[cfg(debug_assertions)]
        web_sys::console::log_1(&"WebSerialTransport: Invoking port.open()...".into());
        let promise = port.open(&serial_options);
        JsFuture::from(promise).await.map_err(|e| {
            // Robust error detection via object properties
            let mut is_invalid_state = false;
            let mut msg = format!("{:?}", e);

            if let Some(obj) = e.dyn_ref::<js_sys::Object>() {
                // Try to get 'name'
                if let Ok(name_val) = js_sys::Reflect::get(obj, &"name".into()) {
                    if let Some(name_str) = name_val.as_string() {
                        if name_str.contains("InvalidStateError") {
                            is_invalid_state = true;
                        }
                    }
                }
                // Update msg from 'message' if available
                if let Ok(m_val) = js_sys::Reflect::get(obj, &"message".into()) {
                    if let Some(m_str) = m_val.as_string() {
                        msg = m_str;
                    }
                }
            } else {
                // Fallback string checks
                let s = format!("{:?}", e);
                if s.contains("InvalidStateError") {
                    is_invalid_state = true;
                }
            }

            if is_invalid_state || msg.to_lowercase().contains("already open") {
                // Map InvalidStateError explicitly to AlreadyOpen when regarding open()
                // (Spec says InvalidStateError = port already open)
                TransportError::AlreadyOpen
            } else {
                TransportError::ConnectionFailed(msg)
            }
        })?;
        #[cfg(debug_assertions)]
        web_sys::console::log_1(&"WebSerialTransport: port.open() resolved.".into());

        // Setup streams
        use wasm_bindgen::JsCast;

        // Readable
        let readable = port.readable();
        // Cast to ReadableStream explicitly
        let stream: web_sys::ReadableStream = readable
            .dyn_into()
            .map_err(|_| TransportError::ConnectionFailed("ReadableStream cast failed".into()))?;

        // get_reader() returns JsValue (Object)
        let reader_val = stream.get_reader();
        let reader: ReadableStreamDefaultReader = reader_val
            .dyn_into()
            .map_err(|_| TransportError::ConnectionFailed("Reader cast failed".into()))?;

        // Writable
        let writable = port.writable();
        let w_stream: web_sys::WritableStream = writable
            .dyn_into()
            .map_err(|_| TransportError::ConnectionFailed("WritableStream cast failed".into()))?;

        let writer_val = w_stream
            .get_writer()
            .map_err(|e| TransportError::ConnectionFailed(format!("get_writer failed: {:?}", e)))?;

        let writer: WritableStreamDefaultWriter = writer_val
            .dyn_into()
            .map_err(|_| TransportError::ConnectionFailed("Writer cast failed".into()))?;

        self.port = Some(port);
        self.reader = Some(reader);
        self.writer = Some(writer);
        self.pending_read = std::cell::RefCell::new(None);
        #[cfg(debug_assertions)]
        web_sys::console::log_1(
            &"WebSerialTransport: Stream readers/writers setup complete.".into(),
        );

        Ok(())
    }

    /// Attach to existing streams (Worker Mode)
    pub fn attach(
        &mut self,
        reader: ReadableStreamDefaultReader,
        writer: WritableStreamDefaultWriter,
    ) {
        self.reader = Some(reader);
        self.writer = Some(writer);
        // Reset pending read
        *self.pending_read.borrow_mut() = None;
        // Port is None in worker mode, signals won't work locally but data will flow
        self.port = None;
    }
}

// Implement the shared Transport trait
impl Transport for WebSerialTransport {
    fn is_open(&self) -> bool {
        self.port.is_some() || self.reader.is_some()
    }

    async fn close(&mut self) -> Result<(), TransportError> {
        // Release locks
        #[cfg(debug_assertions)]
        let start_close = js_sys::Date::now();
        *self.pending_read.borrow_mut() = None; // Drop the promise ref
        if let Some(reader) = self.reader.take() {
            #[cfg(debug_assertions)]
            let start_r = js_sys::Date::now();
            let _ = JsFuture::from(reader.cancel()).await;
            reader.release_lock();
            #[cfg(debug_assertions)]
            {
                let dur_r = js_sys::Date::now() - start_r;
                web_sys::console::log_1(
                    &format!("WebSerialTransport: reader.cancel() took {:.1}ms", dur_r).into(),
                );
            }
        }
        if let Some(writer) = self.writer.take() {
            #[cfg(debug_assertions)]
            let start_w = js_sys::Date::now();

            // CRITICAL FIX: writer.close() can hang indefinitely if device disconnected
            // Use Promise.race() with 100ms timeout to prevent blocking reconnection
            let close_promise = writer.close();
            let timeout_promise = js_sys::Promise::new(&mut |resolve, _reject| {
                if let Some(window) = web_sys::window() {
                    let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(
                        &resolve,
                        WRITER_CLOSE_TIMEOUT_MS,
                    );
                }
            });

            let race_result =
                js_sys::Promise::race(&js_sys::Array::of2(&close_promise, &timeout_promise));
            let _ = JsFuture::from(race_result).await;

            writer.release_lock();
            #[cfg(debug_assertions)]
            {
                let dur_w = js_sys::Date::now() - start_w;
                web_sys::console::log_1(
                    &format!("WebSerialTransport: writer.close() took {:.1}ms", dur_w).into(),
                );
            }
        }

        if let Some(port) = self.port.take() {
            // port.close() via Reflect if binding fails
            if let Ok(func_val) = js_sys::Reflect::get(&port, &"close".into()) {
                if let Ok(func) = func_val.dyn_into::<js_sys::Function>() {
                    // CRITICAL FIX: port.close() can hang or throw if device disconnected
                    // Use timeout and error handling to ensure we don't block
                    match func.call0(&port) {
                        Ok(p) => {
                            #[cfg(debug_assertions)]
                            {
                                web_sys::console::log_1(
                                    &"WebSerialTransport: port.close() invoking...".into(),
                                );
                            }
                            #[cfg(debug_assertions)]
                            let start_c = js_sys::Date::now();

                            // CRITICAL: port.close() can hang if device disconnected
                            // Use timeout to prevent blocking reconnection
                            let close_promise = js_sys::Promise::from(p);
                            let timeout_promise = js_sys::Promise::new(&mut |resolve, _reject| {
                                if let Some(window) = web_sys::window() {
                                    let _ = window
                                        .set_timeout_with_callback_and_timeout_and_arguments_0(
                                            &resolve,
                                            PORT_CLOSE_TIMEOUT_MS,
                                        );
                                }
                            });

                            let race_result = js_sys::Promise::race(&js_sys::Array::of2(
                                &close_promise,
                                &timeout_promise,
                            ));
                            let _ = JsFuture::from(race_result).await;

                            #[cfg(debug_assertions)]
                            {
                                let dur_c = js_sys::Date::now() - start_c;
                                web_sys::console::log_1(
                                    &format!(
                                        "WebSerialTransport: port.close() resolved in {:.1}ms",
                                        dur_c
                                    )
                                    .into(),
                                );
                            }
                        }
                        Err(_) => {
                            // Port already closed or other error - this is fine
                            #[cfg(debug_assertions)]
                            web_sys::console::log_1(
                                &"WebSerialTransport: port.close() call failed (port likely \
                                  already closed)"
                                    .into(),
                            );
                        }
                    }
                }
            }
        }

        #[cfg(debug_assertions)]
        {
            let total_close = js_sys::Date::now() - start_close;
            web_sys::console::log_1(
                &format!(
                    "WebSerialTransport: close() complete in {:.1}ms",
                    total_close
                )
                .into(),
            );
        }
        Ok(())
    }

    async fn read_chunk(&self) -> Result<(Vec<u8>, u64), TransportError> {
        // use f64 for performance.now() timestamp
        let reader = self.reader.as_ref().ok_or(TransportError::NotConnected)?;

        // 1. Get or create read promise
        let read_promise = {
            let mut pending = self.pending_read.borrow_mut();
            if let Some(ref p) = *pending {
                p.clone()
            } else {
                let p = reader.read(); // This returns a promise
                let p_obj: js_sys::Promise = p.unchecked_into();
                *pending = Some(p_obj.clone());
                p_obj
            }
        };

        // 2. Create timeout promise (resolve with "TIMEOUT" string)
        let timeout_string = JsValue::from_str("TIMEOUT");
        let ts_clone = timeout_string.clone();

        let timeout_promise = js_sys::Promise::new(&mut |resolve, _| {
            // Access setTimeout via global scope
            let global = js_sys::global();
            if let Ok(set_timeout) = js_sys::Reflect::get(&global, &"setTimeout".into()) {
                if let Ok(func) = set_timeout.dyn_into::<js_sys::Function>() {
                    // Need to clone for the inner closure because outer is FnMut
                    let ts_inner = ts_clone.clone();

                    // Helper:
                    let callback = wasm_bindgen::closure::Closure::once_into_js(move || {
                        let _ = resolve.call1(&JsValue::NULL, &ts_inner);
                    });

                    // 20ms timeout
                    let _ = func.call2(&JsValue::NULL, &callback, &JsValue::from_f64(20.0));
                }
            }
        });

        // 3. Race
        let race_promise =
            js_sys::Promise::race(&js_sys::Array::of2(&read_promise, &timeout_promise));

        // 4. Await result
        let result_val = JsFuture::from(race_promise)
            .await
            .map_err(|e| TransportError::Io(format!("{:?}", e)))?;

        // 5. Check if timeout
        if result_val == timeout_string {
            // Timeout won. Return empty.
            return Ok((Vec::new(), 0)); // Timestamp ignored for empty
        }

        // 6. Read won. Clear pending read.
        *self.pending_read.borrow_mut() = None;

        let result: js_sys::Object = result_val.unchecked_into();

        // Check done
        let done_val = js_sys::Reflect::get(&result, &"done".into())
            .map_err(|_| TransportError::Io("Invalid read result".into()))?;
        if done_val.as_bool().unwrap_or(false) {
            return Err(TransportError::Io("Stream closed".into()));
        }

        // Get value
        let val = js_sys::Reflect::get(&result, &"value".into())
            .map_err(|_| TransportError::Io("Invalid read result".into()))?;

        let chunk = Uint8Array::new(&val);
        let bytes = chunk.to_vec();

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

        Ok((bytes, (ts_ms * 1000.0) as u64))
    }

    async fn write(&self, data: &[u8]) -> Result<(), TransportError> {
        let writer = self.writer.as_ref().ok_or(TransportError::NotConnected)?;
        let arr = Uint8Array::from(data);

        JsFuture::from(writer.write_with_chunk(&arr))
            .await
            .map_err(|e| TransportError::Io(format!("{:?}", e)))?;

        Ok(())
    }

    async fn set_signals(&self, dtr: bool, rts: bool) -> Result<(), TransportError> {
        let port = self.port.as_ref().ok_or(TransportError::NotConnected)?;

        #[derive(serde::Serialize)]
        #[serde(rename_all = "camelCase")]
        struct Signals {
            data_terminal_ready: bool,
            request_to_send: bool,
        }

        let signals = Signals {
            data_terminal_ready: dtr,
            request_to_send: rts,
        };

        let signals_val = serde_wasm_bindgen::to_value(&signals)
            .map_err(|e| TransportError::Io(format!("Failed to serialize signals: {:?}", e)))?;

        // Use Reflect to get "setSignals" method and call it
        let func_val = js_sys::Reflect::get(port, &"setSignals".into())
            .map_err(|e| TransportError::Io(format!("Failed to get setSignals: {:?}", e)))?;

        let func: js_sys::Function = func_val
            .dyn_into()
            .map_err(|_| TransportError::Io("setSignals is not a function".into()))?;

        let promise_val = func
            .call1(port, &signals_val)
            .map_err(|e| TransportError::Io(format!("setSignals call failed: {:?}", e)))?;

        JsFuture::from(js_sys::Promise::from(promise_val))
            .await
            .map_err(|e| TransportError::Io(format!("{:?}", e)))?;
        Ok(())
    }

    async fn get_signals(&self) -> Result<SignalState, TransportError> {
        let port = self.port.as_ref().ok_or(TransportError::NotConnected)?;

        // getSignals() returns Promise<SerialInputSignals>
        // SerialInputSignals: { dataCarrierDetect, clearToSend, ringIndicator, dataSetReady }

        let func_val = js_sys::Reflect::get(port, &"getSignals".into())
            .map_err(|e| TransportError::Io(format!("Failed to get getSignals: {:?}", e)))?;

        let func: js_sys::Function = func_val
            .dyn_into()
            .map_err(|_| TransportError::Io("getSignals is not a function".into()))?;

        let promise_val = func
            .call0(port)
            .map_err(|e| TransportError::Io(format!("getSignals call failed: {:?}", e)))?;

        let result_val = JsFuture::from(js_sys::Promise::from(promise_val))
            .await
            .map_err(|e| TransportError::Io(format!("{:?}", e)))?;

        // Deserialize result
        // Need a struct to catch JS object
        #[derive(serde::Deserialize)]
        #[allow(non_snake_case)]
        struct JsSignals {
            dataCarrierDetect: bool,
            clearToSend: bool,
            ringIndicator: bool,
            dataSetReady: bool,
        }

        let js_signals: JsSignals = serde_wasm_bindgen::from_value(result_val)
            .map_err(|e| TransportError::Io(format!("Failed to deserialize signals: {:?}", e)))?;

        Ok(SignalState {
            dtr: false, // Output signal, not read from input
            rts: false, // Output signal, not read from input
            dcd: js_signals.dataCarrierDetect,
            dsr: js_signals.dataSetReady,
            ri: js_signals.ringIndicator,
            cts: js_signals.clearToSend,
        })
    }
}
