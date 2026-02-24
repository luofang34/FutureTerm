use crate::context::AppContext;
use crate::protocol::WorkerToUi;
use core_types::RawEvent;
use leptos::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;
use web_sys::{MessageEvent, Worker};

// Data retention limits for the unified raw log
/// Maximum raw log size in bytes (10 MB)
const MAX_LOG_BYTES: usize = 10 * 1024 * 1024;

/// Maximum number of raw log events (safety fallback)
const MAX_LOG_EVENTS: usize = 10000;

/// Maximum number of decoded events to retain
const MAX_DECODED_EVENTS: usize = 2500;

/// Set up the Web Worker and its `onmessage` dispatch callback.
///
/// This creates the worker, wires up the message handler that routes
/// `WorkerToUi` messages to the appropriate signals (raw log, terminal,
/// decoded events, TX forwarding), and stores the worker in the context.
pub fn setup_worker_dispatch(ctx: &AppContext) {
    // Clone all fields we need from AppContext before moving into closures.
    let manager = ctx.manager.clone();
    let set_worker = ctx.set_worker;
    let set_raw_log = ctx.set_raw_log;
    let raw_log_bytes = ctx.raw_log_bytes;
    let set_raw_log_bytes = ctx.set_raw_log_bytes;
    let set_terminal_metadata = ctx.set_terminal_metadata;
    let term_handle = ctx.term_handle;
    let set_events_list = ctx.set_events_list;
    let bridge_active = ctx.bridge_active.clone();
    let bridge_tx_queue = ctx.bridge_tx_queue.clone();
    let needs_session_newline = ctx.needs_session_newline.clone();

    create_effect(move |_| {
        let manager = manager.clone();
        let bridge_active_tx = bridge_active.clone();
        let bridge_tx_queue_tx = bridge_tx_queue.clone();
        let needs_newline = needs_session_newline.clone();
        if let Ok(w) = Worker::new("worker_bootstrap.js") {
            let Ok(decoder) = web_sys::TextDecoder::new() else {
                manager
                    .set_status
                    .set("Failed to create TextDecoder".into());
                return;
            };
            let decode_opts = js_sys::Object::new();
            let _ = js_sys::Reflect::set(&decode_opts, &"stream".into(), &JsValue::from(true));
            let opts: web_sys::TextDecodeOptions = decode_opts.unchecked_into();

            let cb = Closure::wrap(Box::new(move |e: MessageEvent| {
                if let Ok(msg) = serde_wasm_bindgen::from_value::<WorkerToUi>(e.data()) {
                    match msg {
                        WorkerToUi::Status(s) => {
                            // Ignore "Connected" from worker if it's just config confirmation
                            if !s.contains("Worker Ready") {
                                manager.set_status.set(s.clone());
                            }
                        }
                        WorkerToUi::DataBatch { frames, events } => {
                            // Update unified raw log with frames
                            if !frames.is_empty() {
                                set_raw_log.update(|log| {
                                    // Append new raw events and update byte counter
                                    let mut bytes_added = 0;
                                    for frame in &frames {
                                        let event = RawEvent::from_frame(frame);
                                        bytes_added += event.byte_size();
                                        log.push(event);
                                    }

                                    // Update cumulative byte counter
                                    let total_bytes = raw_log_bytes.get_untracked() + bytes_added;
                                    set_raw_log_bytes.set(total_bytes);

                                    if total_bytes > MAX_LOG_BYTES || log.len() > MAX_LOG_EVENTS {
                                        // Trim oldest events until under limit
                                        let mut trimmed = 0;
                                        let mut bytes_removed = 0;

                                        while (total_bytes - bytes_removed > MAX_LOG_BYTES
                                            || log.len() - trimmed > MAX_LOG_EVENTS)
                                            && trimmed < log.len()
                                        {
                                            if let Some(event) = log.get(trimmed) {
                                                bytes_removed += event.byte_size();
                                            }
                                            trimmed += 1;
                                        }

                                        if trimmed > 0 {
                                            log.drain(0..trimmed);

                                            // Update cumulative byte counter after trimming
                                            set_raw_log_bytes.set(total_bytes - bytes_removed);

                                            // Adjust terminal_metadata for the trimmed bytes
                                            set_terminal_metadata.update(|meta| {
                                                meta.adjust_for_log_trim(bytes_removed);
                                            });
                                        }
                                    }
                                });
                            }

                            // Terminal direct write - always write to maintain metadata mapping
                            // Terminal exists even when view is hidden, and we need complete
                            // metadata for cross-view selection sync to work
                            if let Some(term) = term_handle.get_untracked() {
                                // On reconnect, separate from previous session output
                                if needs_newline.get() {
                                    needs_newline.set(false);
                                    term.write("\r\n");
                                    // Keep metadata in sync with the injected newline
                                    set_terminal_metadata.update(|meta| {
                                        meta.record_write(b"\r\n", "\r\n", 0);
                                    });
                                }
                                for f in &frames {
                                    if !f.bytes.is_empty() {
                                        if let Ok(text) = decoder
                                            .decode_with_u8_array_and_options(&f.bytes, &opts)
                                        {
                                            let text: String = text;
                                            if !text.is_empty() {
                                                term.write(&text);

                                                // Record metadata for cross-view selection sync
                                                // This must happen for ALL data, not just when
                                                // Terminal is visible
                                                set_terminal_metadata.update(|meta| {
                                                    meta.record_write(
                                                        &f.bytes,
                                                        &text,
                                                        f.timestamp_us,
                                                    );
                                                });
                                            }
                                        }
                                    }
                                }
                            }

                            // Update events
                            if !events.is_empty() {
                                set_events_list.update(|list| {
                                    list.extend(events);
                                    // Cap at MAX_DECODED_EVENTS to ensure we don't drop
                                    // high-freq MAVLink packets before the View effect can
                                    // process them. 500 was too aggressive for 50Hz streams.
                                    if list.len() > MAX_DECODED_EVENTS {
                                        let split = list.len() - MAX_DECODED_EVENTS;
                                        list.drain(0..split);
                                    }
                                });
                            }
                        }
                        WorkerToUi::AnalyzeResult { baud_rate, score } => {
                            // Received analysis from worker (if we used worker mode)
                            #[cfg(debug_assertions)]
                            web_sys::console::log_1(
                                &format!("Worker Analysis: Baud {} Score {:.2}", baud_rate, score)
                                    .into(),
                            );
                        }
                        WorkerToUi::TxData { data } => {
                            if bridge_active_tx.get() {
                                // Bridge mode - queue for WS send
                                bridge_tx_queue_tx.borrow_mut().push(data);
                            } else {
                                // WebSerial mode
                                let m = manager.clone();
                                spawn_local(async move {
                                    let _ = m.write(&data).await;
                                });
                            }
                        }
                    }
                }
            }) as Box<dyn FnMut(_)>);
            w.set_onmessage(Some(cb.as_ref().unchecked_ref()));
            cb.forget();

            set_worker.set(Some(w));
        } else {
            manager.set_status.set("Failed to spawn worker".into());
        }
    });
}
