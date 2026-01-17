use wasm_bindgen::prelude::*;
use serde_wasm_bindgen::{to_value, from_value};
use web_sys::{MessageEvent, DedicatedWorkerGlobalScope};
use wasm_bindgen_futures::JsFuture;
use crate::protocol::{UiToWorker, WorkerToUi};
use framing::{Framer, lines::LineFramer, raw::RawFramer, cobs_impl::CobsFramer, slip_impl::SlipFramer};
use decoders::{Decoder, hex::HexDecoder};
use std::rc::Rc;
use std::cell::RefCell;
use leptos::logging::log;

// Types
type SharedFramer = Rc<RefCell<Box<dyn Framer>>>;
type SharedDecoder = Rc<RefCell<Box<dyn Decoder>>>;

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
    framer: SharedFramer,
    decoder: SharedDecoder
) {
    let data = event.data();
    
    // Deserialize envelope
    let cmd_val = js_sys::Reflect::get(&data, &"cmd".into()).unwrap_or(data.clone()); 
    
    match from_value::<UiToWorker>(cmd_val) {
        Ok(cmd) => {
            match cmd {
                UiToWorker::IngestData { data, timestamp_us } => {
                    // 1. Frame
                    // 1. Frame
                    let frames_out = framer.borrow_mut().push(&data, timestamp_us);
                     
                     // 2. Decode & Batch
                     let mut events_out = Vec::new();
                     if !frames_out.is_empty() {
                         let mut d = decoder.borrow_mut();
                         for frame in &frames_out {
                             if let Some(event) = d.ingest(frame) {
                                 events_out.push(event);
                             }
                         }
                     }

                     // 3. Send back
                     if !frames_out.is_empty() || !events_out.is_empty() {
                         post_to_ui(&WorkerToUi::DataBatch { frames: frames_out, events: events_out });
                     }
                },
                UiToWorker::Connect { baud_rate } => {
                     log!("Worker: Connect msg received (Config only). Baud: {}", baud_rate);
                     // Just acknowledge. Connection happens on Main Thread now.
                     post_to_ui(&WorkerToUi::Status(format!("Worker Ready (Baud {} - Processor Mode)", baud_rate)));
                },
                UiToWorker::Disconnect => {
                     log!("Worker: Disconnect msg received. Resetting state.");
                     // Reset state
                     framer.borrow_mut().reset();
                     // decoder.borrow_mut().reset(); // Decoder trait doesn't have reset?
                     post_to_ui(&WorkerToUi::Status("Worker State Reset".into()));
                },
                UiToWorker::Send { data: _ } => {
                     post_to_ui(&WorkerToUi::Status("Worker cannot Send (IO on Main Thread)".into()));
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
                UiToWorker::SetFramer { id } => {
                     let mut f = framer.borrow_mut();
                     match id.as_str() {
                         "raw" => *f = Box::new(RawFramer::new()),
                         "lines" => *f = Box::new(LineFramer::new()),
                         "cobs" => *f = Box::new(CobsFramer::new()),
                         "slip" => *f = Box::new(SlipFramer::new()),
                         _ => log!("Unknown framer: {}", id),
                     }
                     post_to_ui(&WorkerToUi::Status(format!("Framer set to {}", id)));
                },
                UiToWorker::Simulate { duration_ms } => {
                      post_to_ui(&WorkerToUi::Status(format!("Starting Simulation ({}ms)...", duration_ms)));
                      let f_metrics = framer.clone();
                      let d_metrics = decoder.clone();
                      
                      wasm_bindgen_futures::spawn_local(async move {
                          let start = js_sys::Date::now();
                          let base_sentence = "$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*47\r\n";
                          while js_sys::Date::now() - start < duration_ms as f64 {
                              // Inject
                              let bytes = base_sentence.as_bytes();
                              let mut f = f_metrics.borrow_mut();
                              let ts = (js_sys::Date::now() * 1000.0) as u64; 
                              let frames = f.push(bytes, ts);
                              drop(f);
                              
                              let mut events = Vec::new();
                              let mut d = d_metrics.borrow_mut();
                              for frame in &frames {
                                  if let Some(event) = d.ingest(frame) {
                                      events.push(event);
                                  }
                              }
                              drop(d);
                              
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
                UiToWorker::AnalyzeRequest { baud_rate, data } => {
                    // Heuristic Logic:
                    // 1. Valid UTF-8 ratio
                    // 2. Control char ratio (avoid binary noise)
                    
                    if data.is_empty() {
                         post_to_ui(&WorkerToUi::AnalyzeResult { baud_rate, score: 0.0 });
                         return;
                    }

                    // Try standard UTF-8 conversion
                    if let Ok(text) = std::str::from_utf8(&data) {
                        // It is valid UTF-8. Evaluate content.
                        let mut printable = 0;
                        let mut control = 0;
                        for c in text.chars() {
                            if c.is_ascii_graphic() || c == ' ' {
                                printable += 1;
                            } else if c.is_ascii_control() && c != '\r' && c != '\n' && c != '\t' {
                                control += 1; // Weird control chars (e.g. 0x01, 0x05) => Bad
                            }
                        }
                        
                        let total = text.chars().count();
                        let printable_ratio = printable as f32 / total as f32;
                        let control_ratio = control as f32 / total as f32;
                        
                        // Scoring function: High printable is good. High Control is bad.
                        let mut score = printable_ratio; 
                        
                        // Penalty for random control chars
                        score -= control_ratio * 2.0;
                        
                        // Penalty for too many replacement chars if we strictly check utf8, but str::from_utf8 ensures validity.
                        
                        // Clamp
                        if score < 0.0 { score = 0.0; }
                        
                        post_to_ui(&WorkerToUi::AnalyzeResult { baud_rate, score });
                    } else {
                        // Invalid UTF-8 => Usually means wrong baud rate (framing errors)
                         post_to_ui(&WorkerToUi::AnalyzeResult { baud_rate, score: 0.0 });
                    }
                },
                _ => {} 

            }
        },
        Err(e) => post_to_ui(&WorkerToUi::Status(format!("Worker Parse Error: {:?}", e))),
    }
}


pub async fn start_worker() {
    log!("Worker: start_worker() BEGIN (Main Thread IO Mode)");
    
    let framer: SharedFramer = Rc::new(RefCell::new(Box::new(RawFramer::new())));
    let decoder: SharedDecoder = Rc::new(RefCell::new(Box::new(HexDecoder::new())));
    
    // Setup message handler
    let f_clone = framer.clone();
    let d_clone = decoder.clone();
    
    let closure = Closure::wrap(Box::new(move |event: MessageEvent| {
        let f = f_clone.clone();
        let d = d_clone.clone();
        
        wasm_bindgen_futures::spawn_local(async move {
            handle_message(event, f, d).await;
        });
    }) as Box<dyn FnMut(_)>);

    worker_scope().set_onmessage(Some(closure.as_ref().unchecked_ref()));
    closure.forget(); // Leak to keep alive
}
