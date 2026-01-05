use wasm_bindgen::prelude::*;
use serde_wasm_bindgen::{to_value, from_value};
use web_sys::{MessageEvent, DedicatedWorkerGlobalScope, SerialPort};
use wasm_bindgen_futures::JsFuture;
use crate::protocol::{UiToWorker, WorkerToUi};
use transport_webserial::{WebSerialTransport, TransportError};
use framing::{Framer, lines::LineFramer, raw::RawFramer};
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
    // post_to_ui(&WorkerToUi::Status("Worker: Message Received".into()));
    
    // Check for "Connect" manually to allow extra fields (streams) which confuse serde
    let connect_val = js_sys::Reflect::get(&data, &"Connect".into());
    
    if let Ok(c_val) = connect_val {
        if !c_val.is_undefined() {
             // It is a Connect message
             // Extract baud_rate from the inner object: { "baud_rate": 123 }
             // But wait, lib.rs sends { "Connect": { "baud_rate": 123 } }
             let baud_obj: js_sys::Object = c_val.unchecked_into();
             let br_val = js_sys::Reflect::get(&baud_obj, &"baud_rate".into()).unwrap_or(JsValue::from(115200));
             // let baud_rate = br_val.as_f64().unwrap_or(115200.0) as u32; // Not needed if we don't use it or open locally?
             // Actually, we perform attach now, baud rate is irrelevant to worker in this mode.
             
             post_to_ui(&WorkerToUi::Status("Worker: Connect Cmd Detected".into()));
             
             let reader_val = js_sys::Reflect::get(&data, &"readable".into()).ok();
             let writer_val = js_sys::Reflect::get(&data, &"writable".into()).ok();
            
             if let (Some(r), Some(w)) = (reader_val, writer_val) {
                 if !r.is_undefined() && !w.is_undefined() {
                     post_to_ui(&WorkerToUi::Status("Worker: Streams extracted".into()));
                     
                     use web_sys::{ReadableStream, WritableStream, ReadableStreamDefaultReader, WritableStreamDefaultWriter};
                     use wasm_bindgen::JsCast;

                     let r_stream: ReadableStream = r.unchecked_into();
                     let w_stream: WritableStream = w.unchecked_into();
                     
                     // Get Reader/Writer from streams
                     let reader_val = r_stream.get_reader();
                     let reader: ReadableStreamDefaultReader = reader_val.unchecked_into();
                     
                     let writer_val = w_stream.get_writer().unwrap(); 
                     let writer: WritableStreamDefaultWriter = writer_val.unchecked_into();
                     
                     let mut t = transport.borrow_mut();
                     t.attach(reader, writer);
                     post_to_ui(&WorkerToUi::Status("Connected".into()));
                     return;
                 } else {
                     post_to_ui(&WorkerToUi::Status("Worker: Streams are undefined".into()));
                 }
            } else {
                 post_to_ui(&WorkerToUi::Status("Worker: Failed to reflect stream properties".into()));
            }
            return;
        }
    }

    match from_value::<UiToWorker>(data.clone()) {
        Ok(cmd) => {
            // log!("Worker parsed cmd: {:?}", cmd);
            match cmd {
                UiToWorker::Connect { .. } => {
                    // Should be handled above manually
                    post_to_ui(&WorkerToUi::Status("Worker: Fallback Connect (Unexpected)".into()));
                },
                UiToWorker::Disconnect => {
                     let mut t = transport.borrow_mut();
                     let _ = t.close().await;
                     post_to_ui(&WorkerToUi::Status("Disconnected".into()));
                },
                UiToWorker::Send { data } => {
                     // Changed to borrow() to allow concurrent Write while Read is pending
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
        // let _is_open = transport.borrow().is_open(); 
        
        // Changed to borrow() to allow concurrent interaction
        // Note: This holds an immutable borrow across the await point.
        let result = transport.borrow().read_chunk().await;
        
        match result {
            Ok((bytes, ts)) => {
                 if !bytes.is_empty() {
                     // Debug: Echo raw bytes to UI status for diagnosis
                     // post_to_ui(&WorkerToUi::Status(format!("RX: {} bytes", bytes.len())));
                     
                     // 1. Frame
                     let mut f = framer.borrow_mut();
                     // timestamp from read is f64 (ms), Framer wants u64 (us).
                     let ts_us = (ts * 1000.0) as u64;
                     let frames = f.push(&bytes, ts_us);
                     
                     if !frames.is_empty() {
                         post_to_ui(&WorkerToUi::Status(format!("RX Framed: {} frames", frames.len())));
                     }
                     
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
    // Use RawFramer for interactive terminal to avoid buffering
    let framer: Rc<RefCell<Box<dyn Framer>>> = Rc::new(RefCell::new(Box::new(RawFramer::new())));
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
