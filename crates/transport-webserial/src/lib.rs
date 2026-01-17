use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{SerialPort, SerialOptions, ReadableStreamDefaultReader, WritableStreamDefaultWriter};
use js_sys::Uint8Array;
use core_types::{Transport, TransportError, SignalState, SerialConfig};

/// WebSerial Transport Implementation.
/// 
/// Note: Usage requires RUSTFLAGS="--cfg=web_sys_unstable_apis"
pub struct WebSerialTransport {
    port: Option<SerialPort>,
    reader: Option<ReadableStreamDefaultReader>,
    writer: Option<WritableStreamDefaultWriter>,
    pending_read: std::cell::RefCell<Option<js_sys::Promise>>,
}

// Safety: WebSerialTransport holds JsValues which are !Send / !Sync.
// However, in a WASM environment (browser), we are single-threaded.
// We implement these traits to satisfy the Transport trait bound,
// assuming this struct will NOT actually be moved across threads 
// (or if it is, the runtime environment handles it via message passing, 
// but direct access would fail if not on the main thread).
unsafe impl Send for WebSerialTransport {}
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

    /// Open the port with specified configuration.
    // Note: Mutability is required here to set self.port, self.reader, self.writer
    pub async fn open(&mut self, port: SerialPort, config: SerialConfig) -> Result<(), TransportError> {
        web_sys::console::log_1(&format!("WebSerialTransport: open() called. Baud: {}", config.baud_rate).into());
        
        let options = js_sys::Object::new();
        let _ = js_sys::Reflect::set(&options, &"baudRate".into(), &JsValue::from(config.baud_rate));
        let _ = js_sys::Reflect::set(&options, &"dataBits".into(), &JsValue::from(config.data_bits));
        let _ = js_sys::Reflect::set(&options, &"stopBits".into(), &JsValue::from(config.stop_bits));
        let _ = js_sys::Reflect::set(&options, &"parity".into(), &JsValue::from(config.parity.as_str()));
        let _ = js_sys::Reflect::set(&options, &"flowControl".into(), &JsValue::from(config.flow_control.as_str()));

        // Convert to SerialOptions
        let serial_options: SerialOptions = options.unchecked_into();
        
        web_sys::console::log_1(&"WebSerialTransport: Invoking port.open()...".into());
        let promise = port.open(&serial_options);
        JsFuture::from(promise).await
            .map_err(|e| TransportError::ConnectionFailed(format!("{:?}", e)))?;
        web_sys::console::log_1(&"WebSerialTransport: port.open() resolved.".into());

        // Setup streams
        use wasm_bindgen::JsCast;

        // Readable
        let readable = port.readable(); 
        // Cast to ReadableStream explicitly
        let stream: web_sys::ReadableStream = readable.dyn_into()
            .map_err(|_| TransportError::ConnectionFailed("ReadableStream cast failed".into()))?;
        
        // get_reader() returns JsValue (Object)
        let reader_val = stream.get_reader();
        let reader: ReadableStreamDefaultReader = reader_val.dyn_into()
             .map_err(|_| TransportError::ConnectionFailed("Reader cast failed".into()))?;
        
        // Writable
        let writable = port.writable();
        let w_stream: web_sys::WritableStream = writable.dyn_into()
            .map_err(|_| TransportError::ConnectionFailed("WritableStream cast failed".into()))?;
            
        let writer_val = w_stream.get_writer()
            .map_err(|e| TransportError::ConnectionFailed(format!("get_writer failed: {:?}", e)))?;
        
        let writer: WritableStreamDefaultWriter = writer_val.dyn_into()
             .map_err(|_| TransportError::ConnectionFailed("Writer cast failed".into()))?;

        self.port = Some(port);
        self.reader = Some(reader);
        self.writer = Some(writer);
        self.pending_read = std::cell::RefCell::new(None);
        web_sys::console::log_1(&"WebSerialTransport: Stream readers/writers setup complete.".into());

        Ok(())
    }

    /// Attach to existing streams (Worker Mode)
    pub fn attach(&mut self, reader: ReadableStreamDefaultReader, writer: WritableStreamDefaultWriter) {
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
        let start_close = js_sys::Date::now();
        *self.pending_read.borrow_mut() = None; // Drop the promise ref
        if let Some(reader) = self.reader.take() {
            let start_r = js_sys::Date::now();
            let _ = JsFuture::from(reader.cancel()).await;
            reader.release_lock();
            let dur_r = js_sys::Date::now() - start_r;
            web_sys::console::log_1(&format!("WebSerialTransport: reader.cancel() took {:.1}ms", dur_r).into());
        }
        if let Some(writer) = self.writer.take() {
            let start_w = js_sys::Date::now();
            let _ = JsFuture::from(writer.close()).await;
            writer.release_lock();
            let dur_w = js_sys::Date::now() - start_w;
            web_sys::console::log_1(&format!("WebSerialTransport: writer.close() took {:.1}ms", dur_w).into());
        }
        
        if let Some(port) = self.port.take() {
            // port.close() via Reflect if binding fails
            let _ = match js_sys::Reflect::get(&port, &"close".into()) {
                Ok(func_val) => {
                     if let Ok(func) = func_val.dyn_into::<js_sys::Function>() {
                         let promise = func.call0(&port);
                         if let Ok(p) = promise {
                             web_sys::console::log_1(&"WebSerialTransport: port.close() invoking...".into());
                             let start_c = js_sys::Date::now();
                             JsFuture::from(js_sys::Promise::from(p)).await.ok();
                             let dur_c = js_sys::Date::now() - start_c;
                             web_sys::console::log_1(&format!("WebSerialTransport: port.close() resolved in {:.1}ms", dur_c).into());
                         }
                     }
                },
                Err(_) => {}
            };
        }
        
        let total_close = js_sys::Date::now() - start_close;
        web_sys::console::log_1(&format!("WebSerialTransport: close() complete in {:.1}ms", total_close).into());
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
        let race_promise = js_sys::Promise::race(&js_sys::Array::of2(&read_promise, &timeout_promise));
        
        // 4. Await result
        let result_val = JsFuture::from(race_promise).await
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
        let perf_val = js_sys::Reflect::get(&global, &"performance".into())
             .unwrap_or(JsValue::UNDEFINED);
        
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
        
        JsFuture::from(writer.write_with_chunk(&arr)).await
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
             
         let func: js_sys::Function = func_val.dyn_into()
             .map_err(|_| TransportError::Io("setSignals is not a function".into()))?;
             
         let promise_val = func.call1(port, &signals_val)
             .map_err(|e| TransportError::Io(format!("setSignals call failed: {:?}", e)))?;
             
         JsFuture::from(js_sys::Promise::from(promise_val)).await
            .map_err(|e| TransportError::Io(format!("{:?}", e)))?;
         Ok(())
    }

    async fn get_signals(&self) -> Result<SignalState, TransportError> {
        let port = self.port.as_ref().ok_or(TransportError::NotConnected)?;
        
        // getSignals() returns Promise<SerialInputSignals>
        // SerialInputSignals: { dataCarrierDetect, clearToSend, ringIndicator, dataSetReady }
        
        let func_val = js_sys::Reflect::get(port, &"getSignals".into())
             .map_err(|e| TransportError::Io(format!("Failed to get getSignals: {:?}", e)))?;
             
        let func: js_sys::Function = func_val.dyn_into()
             .map_err(|_| TransportError::Io("getSignals is not a function".into()))?;
             
        let promise_val = func.call0(port)
             .map_err(|e| TransportError::Io(format!("getSignals call failed: {:?}", e)))?;
             
        let result_val = JsFuture::from(js_sys::Promise::from(promise_val)).await
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
            ri:  js_signals.ringIndicator,
            cts: js_signals.clearToSend,
        })
    }
}
