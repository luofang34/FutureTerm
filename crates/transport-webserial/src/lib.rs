use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{SerialPort, SerialOptions, ReadableStreamDefaultReader, WritableStreamDefaultWriter};
use js_sys::{Uint8Array, Array};
use core_types::Frame;

/// WebSerial Transport Implementation.
/// 
/// Note: Usage requires RUSTFLAGS="--cfg=web_sys_unstable_apis"
pub struct WebSerialTransport {
    port: Option<SerialPort>,
    reader: Option<ReadableStreamDefaultReader>,
    writer: Option<WritableStreamDefaultWriter>,
}

#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    #[error("WebSerial API not supported or disabled")]
    NotSupported,
    #[error("Failed to open port: {0}")]
    OpenFailed(String),
    #[error("Port not open")]
    NotOpen,
    #[error("IO Error: {0}")]
    Io(String),
}

impl WebSerialTransport {
    pub fn new() -> Self {
        Self {
            port: None,
            reader: None,
            writer: None,
        }
    }

    /// Open the port with specified baud rate.
    pub async fn open(&mut self, port: SerialPort, baud_rate: u32) -> Result<(), TransportError> {
        let options = SerialOptions::new(baud_rate);
        
        let promise = port.open(&options);
        JsFuture::from(promise).await
            .map_err(|e| TransportError::OpenFailed(format!("{:?}", e)))?;

        // Setup streams
        use wasm_bindgen::JsCast;

        // Readable
        let readable = port.readable(); 
        // Cast to ReadableStream explicitly
        let stream: web_sys::ReadableStream = readable.dyn_into()
            .map_err(|_| TransportError::OpenFailed("ReadableStream cast failed".into()))?;
        
        // get_reader() returns JsValue (Object)
        let reader_val = stream.get_reader();
        let reader: ReadableStreamDefaultReader = reader_val.dyn_into()
             .map_err(|_| TransportError::OpenFailed("Reader cast failed".into()))?;
        
        // Writable
        let writable = port.writable();
        let w_stream: web_sys::WritableStream = writable.dyn_into()
            .map_err(|_| TransportError::OpenFailed("WritableStream cast failed".into()))?;
            
        let writer_val = w_stream.get_writer()
            .map_err(|e| TransportError::OpenFailed(format!("get_writer failed: {:?}", e)))?;
        
        let writer: WritableStreamDefaultWriter = writer_val.dyn_into()
             .map_err(|_| TransportError::OpenFailed("Writer cast failed".into()))?;

        self.port = Some(port);
        self.reader = Some(reader);
        self.writer = Some(writer);

        Ok(())
    }

    pub async fn close(&mut self) -> Result<(), TransportError> {
        // Release locks
        if let Some(reader) = self.reader.take() {
            let _ = JsFuture::from(reader.cancel()).await;
            reader.release_lock();
        }
        if let Some(writer) = self.writer.take() {
            let _ = JsFuture::from(writer.close()).await;
            writer.release_lock();
        }
        
        if let Some(port) = self.port.take() {
            let _ = JsFuture::from(port.close()).await;
        }
        Ok(())
    }

    pub async fn read_chunk(&mut self) -> Result<(Vec<u8>, f64), TransportError> {
        // use f64 for performance.now() timestamp
        let reader = self.reader.as_ref().ok_or(TransportError::NotOpen)?;
        
        // Read is { value: Uint8Array, done: bool }
        let result_val = JsFuture::from(reader.read()).await
            .map_err(|e| TransportError::Io(format!("{:?}", e)))?;
        
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
        
        // Get timestamp (browser perf time)
        let window = web_sys::window().expect("Window");
        let ts = window.performance().expect("Performance").now();

        Ok((bytes, ts))
    }

    pub async fn write(&mut self, data: &[u8]) -> Result<(), TransportError> {
        let writer = self.writer.as_ref().ok_or(TransportError::NotOpen)?;
        let arr = Uint8Array::from(data);
        
        JsFuture::from(writer.write_with_chunk(&arr)).await
            .map_err(|e| TransportError::Io(format!("{:?}", e)))?;
            
        Ok(())
    }
    
    pub async fn set_signals(&mut self, dtr: bool, rts: bool) -> Result<(), TransportError> {
         let port = self.port.as_ref().ok_or(TransportError::NotOpen)?;
         // PortSignal 
         let signals = web_sys::SerialOutputSignals::new();
         signals.set_data_terminal_ready(dtr);
         signals.set_request_to_send(rts);
         
         // Use Reflect to get "setSignals" method and call it
         let func_val = js_sys::Reflect::get(port, &"setSignals".into())
             .map_err(|e| TransportError::Io(format!("Failed to get setSignals: {:?}", e)))?;
             
         let func: js_sys::Function = func_val.dyn_into()
             .map_err(|_| TransportError::Io("setSignals is not a function".into()))?;
             
         let promise_val = func.call1(port, &signals)
             .map_err(|e| TransportError::Io(format!("setSignals call failed: {:?}", e)))?;
             
         JsFuture::from(js_sys::Promise::from(promise_val)).await
            .map_err(|e| TransportError::Io(format!("{:?}", e)))?;
         Ok(())
    }
}
