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
        #[cfg(feature = "mavlink")]
        "mavlink" => Some(Box::new(decoders::mavlink::MavlinkDecoder::new())),
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

// We need to change the signature of handle_message to accept the heartbeat flag
async fn handle_message(
    event: MessageEvent,
    framer: SharedFramer,
    decoder: SharedDecoder,
    hb_active: Rc<RefCell<bool>>,
) {
    let data = event.data();
    if let Ok(cmd) = from_value::<UiToWorker>(data) { match cmd {
        UiToWorker::Connect { baud_rate } => {
            log!("Worker: Connect command received (Baud: {})", baud_rate);
        }
        UiToWorker::StartHeartbeat => {
            let mut active = hb_active.borrow_mut();
            if !*active {
                // Only start if not already active
                *active = true;
                log!("Worker: Starting Heartbeat Loop (1Hz)");

                let hb_flag = hb_active.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let mut seq = 0u8;
                    while *hb_flag.borrow() {
                        #[cfg(feature = "mavlink")]
                        {
                            use mavlink::common::MavMessage;
                            use mavlink::MavHeader;

                            let header = MavHeader {
                                system_id: 255,
                                component_id: 190,
                                sequence: seq,
                            };

                            let data = mavlink::common::HEARTBEAT_DATA {
                                custom_mode: 0,
                                mavtype: mavlink::common::MavType::MAV_TYPE_GCS,
                                autopilot: mavlink::common::MavAutopilot::MAV_AUTOPILOT_INVALID,
                                base_mode: mavlink::common::MavModeFlag::empty(),
                                system_status: mavlink::common::MavState::MAV_STATE_ACTIVE,
                                mavlink_version: 3,
                            };

                            let msg = MavMessage::HEARTBEAT(data);
                            let mut buf = Vec::new();
                            // Send v1 for max compatibility
                            if mavlink::write_v1_msg(&mut buf, header, &msg).is_ok() {
                                post_to_ui(&WorkerToUi::TxData { data: buf });
                            }
                        }
                        #[cfg(not(feature = "mavlink"))]
                        {
                            // No-op if disabled
                        }

                        // Sleep 1s
                        let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(
                            &mut |r, _| {
                                let _ = worker_scope()
                                    .set_timeout_with_callback_and_timeout_and_arguments_0(
                                        &r, 1000,
                                    );
                            },
                        ))
                        .await;

                        seq = seq.wrapping_add(1);
                    }
                    log!("Worker: Heartbeat Loop Exited");
                });
            } else {
                log!("Worker: Heartbeat already active");
            }
        }
        UiToWorker::StopHeartbeat => {
            *hb_active.borrow_mut() = false;
            // log!("Worker: Stopping Heartbeat");
        }
        UiToWorker::IngestData { data, timestamp_us } => {
            // 1. Frame
            // log!("Worker: Ingesting {} bytes", data.len());
            let frames_out = framer.borrow_mut().push(&data, timestamp_us);

            // 2. Decode & Batch
            let mut events_out = Vec::new();
            if !frames_out.is_empty() {
                let mut d = decoder.borrow_mut();
                for frame in &frames_out {
                    // log!("Worker: Ingesting frame len={}", frame.bytes.len());
                    d.ingest(frame, &mut events_out);
                }
            } else {
                // log!("Worker: IngestData received {} bytes but produced 0 frames", data.len());
            }

            // 3. Send back
            if !frames_out.is_empty() || !events_out.is_empty() {
                // if !events_out.is_empty() { log!("Worker: Sending {} events", events_out.len()); }
                post_to_ui(&WorkerToUi::DataBatch {
                    frames: frames_out,
                    events: events_out,
                });
            }
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
        UiToWorker::AnalyzeRequest { baud_rate, data } => {
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
        UiToWorker::SetSignals { dtr, rts } => {
            log!("Worker: SetSignals DTR={} RTS={}", dtr, rts);
        }
        // Catch-all for other variants to match exhaustively
        _ => {
            // Ignore others or log
        }
    } }
}

pub async fn start_worker() {
    log!("Worker: start_worker() BEGIN (Main Thread IO Mode)");

    let framer: SharedFramer = Rc::new(RefCell::new(Box::new(RawFramer::new())));
    // Default to Utf8 for ANSI colors
    let decoder: SharedDecoder = Rc::new(RefCell::new(Box::new(Utf8Decoder::new())));

    // Heartbeat Active Flag (Shared across closure invocations)
    let hb_active = Rc::new(RefCell::new(false));

    // Setup message handler
    let f_clone = framer.clone();
    let d_clone = decoder.clone();

    let closure = Closure::wrap(Box::new(move |event: MessageEvent| {
        let f = f_clone.clone();
        let d = d_clone.clone();
        let hb = hb_active.clone();

        wasm_bindgen_futures::spawn_local(async move {
            handle_message(event, f, d, hb).await;
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
        #[cfg(feature = "mavlink")]
        assert!(create_decoder("mavlink").is_some());
        assert!(create_decoder("utf8").is_some());
    }

    #[test]
    fn test_create_decoder_invalid() {
        assert!(create_decoder("unknown").is_none());
    }
}
