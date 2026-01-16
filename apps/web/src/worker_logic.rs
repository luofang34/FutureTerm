use wasm_bindgen::prelude::*;
use serde_wasm_bindgen::{to_value, from_value};
use web_sys::{MessageEvent, DedicatedWorkerGlobalScope, SerialPort};
use wasm_bindgen_futures::JsFuture;
use crate::protocol::{UiToWorker, WorkerToUi};
use transport_webserial::WebSerialTransport;
use core_types::{Transport, TransportError};
use framing::{Framer, lines::LineFramer};
use decoders::{Decoder, hex::HexDecoder};
use std::rc::Rc;
use std::cell::RefCell;
use leptos::logging::log;

// Types
type SharedTransport = Rc<RefCell<WebSerialTransport>>;

fn worker_scope() -> DedicatedWorkerGlobalScope {
    js_sys::global().unchecked_into()
}

fn post_to_ui(msg: &WorkerToUi) {
    let scope = worker_scope();
    if let Ok(val) = to_value(msg) {
        let _ = scope.post_message(&val);
    }
}

// Handler for incoming messages from UI
async fn handle_message(
    event: MessageEvent, 
    transport: SharedTransport,
    framer: Rc<RefCell<Box<dyn Framer>>>,
    decoder: Rc<RefCell<Box<dyn Decoder>>>
) {
    let data = event.data();
    // Notify UI we got something (Debugging)
    post_to_ui(&WorkerToUi::Status("Worker: Message Received".into()));
    
    // Extract envelope: { cmd: ..., port: ... }
    let cmd_val = js_sys::Reflect::get(&data, &"cmd".into()).unwrap_or(data.clone()); 
    
    match from_value::<UiToWorker>(cmd_val) {
        Ok(cmd) => {
            web_sys::console::log_1(&format!("Worker parsed cmd: {:?}", cmd).into());
            match cmd {
                UiToWorker::Connect { baud_rate } => {
                    web_sys::console::log_1(&"Worker: Handling Connect...".into());
                    // Port is on the envelope (data), not inside cmd
                    let port_val = js_sys::Reflect::get(&data, &"port".into()).ok();
                    if let Some(val) = port_val {
                        if !val.is_undefined() && !val.is_null() {
                             web_sys::console::log_1(&"Worker: Port found in message.".into());
                             let port: SerialPort = val.unchecked_into();
                             // Must be mutable because open() takes &mut self
                             let mut t = transport.borrow_mut();
                             web_sys::console::log_1(&"Worker: Calling transport.open...".into());
                             match t.open(port, baud_rate).await {
                                 Ok(_) => {
                                     web_sys::console::log_1(&"Worker: Transport opened.".into());
                                     post_to_ui(&WorkerToUi::Status("Connected".into()));
                                 },
                                 Err(e) => {
                                     web_sys::console::log_1(&format!("Worker: Transport open failed: {:?}", e).into());
                                     post_to_ui(&WorkerToUi::Status(format!("Error: {:?}", e)));
                                 }
                             }
                             return;
                        }
                    }
                    web_sys::console::log_1(&"Worker: Port NOT found in message.".into());
                    post_to_ui(&WorkerToUi::Status("Error: Connect received without port".into()));
                },
                UiToWorker::Disconnect => {
                     // close() takes &mut self
                     let mut t = transport.borrow_mut();
                     let _ = t.close().await;
                     post_to_ui(&WorkerToUi::Status("Disconnected".into()));
                },
                UiToWorker::Send { data } => {
                     // write() takes &self, so no mut needed for the transport struct itself?
                     // Wait, RefCell::borrow_mut() returns a RefMut, which derefs to &mut T.
                     // If write() takes &self, we can use borrow() instead of borrow_mut()!
                     // BUT, we used SharedTransport = Rc<RefCell<WebSerialTransport>>.
                     // If we want to allow concurrent reads, we should use borrow().
                     // But WebSerialTransport logic might require internal mutability if it wasn't using RefCell? 
                     // No, WebSerialTransport struct fields are Option<...>. Setting them requires &mut.
                     // Writing to them: writer.write_with_chunk takes &self (JS API).
                     
                     // So we can use borrow() here if write() is &self.
                     let t = transport.borrow();
                     let _ = t.write(&data).await;
                },
                UiToWorker::SetDecoder { id } => {
                     let mut d = decoder.borrow_mut();
                     match id.as_str() {
                         "hex" => *d = Box::new(decoders::hex::HexDecoder::new()),
                         "nmea" => *d = Box::new(decoders::nmea::NmeaDecoder::new()),
                         _ => log!("Unknown decoder: {}", id),
                     }
                     post_to_ui(&WorkerToUi::Status(format!("Decoder set to {}", id)));
                },
                UiToWorker::Simulate { duration_ms } => {
                     post_to_ui(&WorkerToUi::Status(format!("Starting Simulation ({}ms)...", duration_ms)));
                     let f_metrics = framer.clone();
                     let d_metrics = decoder.clone();
                     
                     wasm_bindgen_futures::spawn_local(async move {
                         let start = js_sys::Date::now();
                         // GPGGA sample
                         let base_sentence = "$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*47\r\n";
                         
                         while js_sys::Date::now() - start < duration_ms as f64 {
                             // Inject
                             let bytes = base_sentence.as_bytes();
                             let mut f = f_metrics.borrow_mut();
                             // Mock timestamp
                             let ts = (js_sys::Date::now() * 1000.0) as u64; 
                             let frames = f.push(bytes, ts);
                             drop(f); // Release borrow
                             
                             let mut events = Vec::new();
                             let mut d = d_metrics.borrow_mut();
                             for frame in &frames {
                                 if let Some(event) = d.ingest(frame) {
                                     events.push(event);
                                 }
                             }
                             drop(d); // Release borrow
                             
                             if !frames.is_empty() {
                                 post_to_ui(&WorkerToUi::DataBatch { frames, events });
                             }
                             
                             // Sleep 500ms
                             let _ = JsFuture::from(
                                 js_sys::Promise::new(&mut |resolve, _| {
                                     let _ = worker_scope().set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 500);
                                 })
                             ).await;
                         }
                         post_to_ui(&WorkerToUi::Status("Simulation Complete".into()));
                     });
                },
                _ => log!("Unhandled command"),
            }
        },
        Err(e) => post_to_ui(&WorkerToUi::Status(format!("Worker Parse Error: {:?}", e))),
    }
}

// The loop that reads from transport and feeds framer
async fn read_loop(
    transport: SharedTransport,
    framer: Rc<RefCell<Box<dyn Framer>>>,
    decoder: Rc<RefCell<Box<dyn Decoder>>>
) {
    loop {
        // Yield to allow message handling? 
        // JS is event loop based. We need to be careful not to block strictly.
        // But `read_chunk` is async, so it yields.
        
        // Check if connected
        // borrow() is enough for is_open()
        let _is_open = transport.borrow().is_open(); 
        
        // read_chunk uses &self in WebSerialTransport, so borrow() is fine
        let result = transport.borrow().read_chunk().await;
        
        match result {
            Ok((bytes, ts)) => {
                 if !bytes.is_empty() {
                     // 1. Frame
                     let mut f = framer.borrow_mut();
                     // timestamp from read is f64 (ms), Framer wants u64 (us).
                     let ts_us = ts;
                     let frames = f.push(&bytes, ts_us);
                     
                     // 2. Decode & Batch
                     let mut events = Vec::new();
                     let mut d = decoder.borrow_mut();
                     for frame in &frames {
                         if let Some(event) = d.ingest(frame) {
                             events.push(event);
                         }
                     }
                     
                     // 3. Send to UI
                     // Optimize: Only send if not empty
                     if !frames.is_empty() {
                         post_to_ui(&WorkerToUi::DataBatch { frames, events });
                     }
                 }
            },
            Err(TransportError::Io(e)) => {
                // Serious IO error (unplugged?), notify
                post_to_ui(&WorkerToUi::Status(format!("IO Error: {}", e)));
                // Avoid spin loop if error persists instantly?
                // Add delay
                let _ = JsFuture::from(js_sys::Promise::resolve(&JsValue::UNDEFINED)).await; 
            },
            Err(_) => {
                // Not open, just wait a bit (polling mode essentially if we don't have signal)
                // In a real worker, we ideally wait on a "Connected" signal or just sleep.
                // 100ms sleep
                let promise = js_sys::Promise::new(&mut |resolve, _| {
                    let _ = worker_scope().set_timeout_with_callback_and_timeout_and_arguments_0(&resolve, 100);
                });
                let _ = JsFuture::from(promise).await;
            }
        }
    }
}

pub async fn start_worker() {
    let transport = Rc::new(RefCell::new(WebSerialTransport::new()));
    let framer: Rc<RefCell<Box<dyn Framer>>> = Rc::new(RefCell::new(Box::new(LineFramer::new())));
    let decoder: Rc<RefCell<Box<dyn Decoder>>> = Rc::new(RefCell::new(Box::new(HexDecoder::new())));
    
    // Setup message handler
    let t_clone = transport.clone();
    let f_clone = framer.clone();
    let d_clone = decoder.clone();
    
    let closure = Closure::wrap(Box::new(move |event: MessageEvent| {
        // Clone for async block
        let t = t_clone.clone(); 
        let f = f_clone.clone();
        let d = d_clone.clone();
        
        wasm_bindgen_futures::spawn_local(async move {
            handle_message(event, t, f, d).await;
        });
    }) as Box<dyn FnMut(_)>);

    worker_scope().set_onmessage(Some(closure.as_ref().unchecked_ref()));
    closure.forget(); // Leak to keep alive

    // Start read loop
    read_loop(transport, framer, decoder).await;
}
