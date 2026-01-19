use crate::protocol::{UiToWorker, WorkerToUi};
use decoders::{utf8::Utf8Decoder, Decoder};
use framing::{
    cobs_impl::CobsFramer, lines::LineFramer, raw::RawFramer, slip_impl::SlipFramer, Framer,
};
use leptos::logging::log;
use serde_wasm_bindgen::{from_value, to_value};
use std::cell::RefCell;
use std::rc::Rc;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use web_sys::{DedicatedWorkerGlobalScope, MessageEvent};

// Types
type SharedFramer = Rc<RefCell<Box<dyn Framer>>>;
type SharedDecoder = Rc<RefCell<Box<dyn Decoder>>>;

/// Factory function to create a framer by ID
pub fn create_framer(id: &str) -> Option<Box<dyn Framer>> {
    match id {
        "raw" => Some(Box::new(RawFramer::new())),
        "lines" => Some(Box::new(LineFramer::new())),
        "cobs" => Some(Box::new(CobsFramer::new())),
        "slip" => Some(Box::new(SlipFramer::new())),
        _ => None,
    }
}

/// Factory function to create a decoder by ID
pub fn create_decoder(id: &str) -> Option<Box<dyn Decoder>> {
    match id {
        "hex" => Some(Box::new(decoders::hex::HexDecoder::new())),
        "nmea" => Some(Box::new(decoders::nmea::NmeaDecoder::new())),
        "utf8" => Some(Box::new(Utf8Decoder::new())),
        _ => None,
    }
}

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
async fn handle_message(event: MessageEvent, framer: SharedFramer, decoder: SharedDecoder) {
    let data = event.data();

    // Direct deserialization (Legacy envelope logic removed)
    match from_value::<UiToWorker>(data) {
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
                        post_to_ui(&WorkerToUi::DataBatch {
                            frames: frames_out,
                            events: events_out,
                        });
                    }
                }
                UiToWorker::Connect { baud_rate } => {
                    log!(
                        "Worker: Connect msg received (Config only). Baud: {}",
                        baud_rate
                    );
                    // Just acknowledge. Connection happens on Main Thread now.
                    post_to_ui(&WorkerToUi::Status(format!(
                        "Worker Ready (Baud {} - Processor Mode)",
                        baud_rate
                    )));
                }
                UiToWorker::Disconnect => {
                    log!("Worker: Disconnect msg received. Resetting state.");
                    // Reset state
                    framer.borrow_mut().reset();
                    // decoder.borrow_mut().reset(); // Decoder trait doesn't have reset?
                    post_to_ui(&WorkerToUi::Status("Worker State Reset".into()));
                }
                UiToWorker::Send { data: _ } => {
                    post_to_ui(&WorkerToUi::Status(
                        "Worker cannot Send (IO on Main Thread)".into(),
                    ));
                }
                UiToWorker::SetDecoder { id } => {
                    if let Some(new_decoder) = create_decoder(&id) {
                        *decoder.borrow_mut() = new_decoder;
                        post_to_ui(&WorkerToUi::Status(format!("Decoder set to {}", id)));
                    } else {
                        log!("Unknown decoder: {}", id);
                    }
                }
                UiToWorker::SetFramer { id } => {
                    if let Some(new_framer) = create_framer(&id) {
                        *framer.borrow_mut() = new_framer;
                        post_to_ui(&WorkerToUi::Status(format!("Framer set to {}", id)));
                    } else {
                        log!("Unknown framer: {}", id);
                    }
                }
                UiToWorker::Simulate { duration_ms } => {
                    post_to_ui(&WorkerToUi::Status(format!(
                        "Starting Simulation ({}ms)...",
                        duration_ms
                    )));
                    let f_metrics = framer.clone();
                    let d_metrics = decoder.clone();

                    #[allow(clippy::await_holding_refcell_ref)]
                    wasm_bindgen_futures::spawn_local(async move {
                        let start = js_sys::Date::now();
                        let base_sentence =
                            "$GPGGA,123519,4807.038,N,01131.000,E,1,08,0.9,545.4,M,46.9,M,,*47\r\n";
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
                            let _ = JsFuture::from(js_sys::Promise::new(&mut |resolve, _| {
                                let _ = worker_scope()
                                    .set_timeout_with_callback_and_timeout_and_arguments_0(
                                        &resolve, 500,
                                    );
                            }))
                            .await;
                        }
                        post_to_ui(&WorkerToUi::Status("Simulation Complete".into()));
                    });
                }
                UiToWorker::AnalyzeRequest { baud_rate, data } => {
                    // Heuristic Logic (Using Shared Analysis Crate):
                    // 1. Valid UTF-8 / ASCII ratio

                    if data.is_empty() {
                        post_to_ui(&WorkerToUi::AnalyzeResult {
                            baud_rate,
                            score: 0.0,
                        });
                        return;
                    }

                    let score = analysis::calculate_score_8n1(&data);
                    post_to_ui(&WorkerToUi::AnalyzeResult { baud_rate, score });
                }
                _ => {}
            }
        }
        Err(e) => post_to_ui(&WorkerToUi::Status(format!("Worker Parse Error: {:?}", e))),
    }
}

pub async fn start_worker() {
    log!("Worker: start_worker() BEGIN (Main Thread IO Mode)");

    let framer: SharedFramer = Rc::new(RefCell::new(Box::new(RawFramer::new())));
    // Default to Utf8 for ANSI colors
    let decoder: SharedDecoder = Rc::new(RefCell::new(Box::new(Utf8Decoder::new())));

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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_create_framer_valid() {
        assert!(create_framer("raw").is_some());
        assert!(create_framer("lines").is_some());
        assert!(create_framer("cobs").is_some());
        assert!(create_framer("slip").is_some());
    }

    #[test]
    fn test_create_framer_invalid() {
        assert!(create_framer("unknown").is_none());
    }

    #[test]
    fn test_create_decoder_valid() {
        assert!(create_decoder("hex").is_some());
        assert!(create_decoder("nmea").is_some());
        assert!(create_decoder("utf8").is_some());
    }

    #[test]
    fn test_create_decoder_invalid() {
        assert!(create_decoder("unknown").is_none());
    }
}
