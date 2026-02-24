use crate::bridge_context::BridgeContext;
use crate::context::AppContext;
use crate::protocol::WorkerToUi;
use core_types::RawEvent;
use leptos::*;
use std::collections::VecDeque;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

use web_sys::{MessageEvent, Worker};

// Data retention limits for the unified raw log
/// Maximum raw log size in bytes (10 MB)
const MAX_LOG_BYTES: usize = 10 * 1024 * 1024;

/// Maximum number of raw log events (safety fallback)
const MAX_LOG_EVENTS: usize = 10000;

/// Maximum number of decoded events to retain
const MAX_DECODED_EVENTS: usize = 2500;

/// Trim raw_log events from the front to stay under size/count limits.
/// Returns (events_trimmed, bytes_removed).
fn trim_raw_log(
    log: &mut VecDeque<RawEvent>,
    total_bytes: usize,
    max_bytes: usize,
    max_events: usize,
) -> (usize, usize) {
    let mut trimmed = 0;
    let mut bytes_removed = 0;

    while (total_bytes - bytes_removed > max_bytes || log.len() - trimmed > max_events)
        && trimmed < log.len()
    {
        if let Some(event) = log.get(trimmed) {
            bytes_removed += event.byte_size();
        }
        trimmed += 1;
    }

    if trimmed > 0 {
        log.drain(0..trimmed);
    }

    (trimmed, bytes_removed)
}

/// Trim decoded events list to stay under capacity.
/// Removes oldest events from the front.
fn trim_decoded_events<T>(events: &mut VecDeque<T>, max_events: usize) {
    if events.len() > max_events {
        let split = events.len() - max_events;
        events.drain(0..split);
    }
}

/// Set up the Web Worker and its `onmessage` dispatch callback.
///
/// This creates the worker, wires up the message handler that routes
/// `WorkerToUi` messages to the appropriate signals (raw log, terminal,
/// decoded events, TX forwarding), and stores the worker in the context.
pub fn setup_worker_dispatch(ctx: &AppContext, bctx: &BridgeContext) {
    // Clone all fields we need from AppContext/BridgeContext before moving into closures.
    let manager = ctx.manager.clone();
    let set_worker = ctx.set_worker;
    let set_raw_log = ctx.set_raw_log;
    let raw_log_bytes = ctx.raw_log_bytes;
    let set_raw_log_bytes = ctx.set_raw_log_bytes;
    let set_terminal_metadata = ctx.set_terminal_metadata;
    let term_handle = ctx.term_handle;
    let set_events_list = ctx.set_events_list;
    let needs_session_newline = bctx.needs_session_newline.clone();

    create_effect(move |_| {
        let manager = manager.clone();
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
                                        log.push_back(event);
                                    }

                                    // Update cumulative byte counter
                                    let total_bytes = raw_log_bytes.get_untracked() + bytes_added;
                                    set_raw_log_bytes.set(total_bytes);

                                    if total_bytes > MAX_LOG_BYTES || log.len() > MAX_LOG_EVENTS {
                                        let (_trimmed, bytes_removed) = trim_raw_log(
                                            log,
                                            total_bytes,
                                            MAX_LOG_BYTES,
                                            MAX_LOG_EVENTS,
                                        );

                                        if bytes_removed > 0 {
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
                                    trim_decoded_events(list, MAX_DECODED_EVENTS);
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
                            manager.send_tx(data);
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

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use core_types::{Channel, RawEvent};
    use std::collections::VecDeque;

    // Helper to create a test RawEvent with a given byte size
    fn make_event(size: usize) -> RawEvent {
        RawEvent::new(0, Channel::Rx, vec![0u8; size])
    }

    // Helper to create a test RawEvent with a specific timestamp
    fn make_event_with_ts(size: usize, ts: u64) -> RawEvent {
        RawEvent::new(ts, Channel::Rx, vec![0u8; size])
    }

    // --- trim_raw_log tests ---

    #[test]
    fn test_trim_raw_log_under_limits() {
        let mut log = VecDeque::from(vec![make_event(100), make_event(200)]);
        let total_bytes = 300;
        let (trimmed, bytes_removed) = trim_raw_log(&mut log, total_bytes, 1000, 10);
        assert_eq!(trimmed, 0);
        assert_eq!(bytes_removed, 0);
        assert_eq!(log.len(), 2);
    }

    #[test]
    fn test_trim_raw_log_over_byte_limit() {
        // 5 events of 300 bytes each = 1500 total, max_bytes = 1000
        let mut log: VecDeque<RawEvent> = (0..5).map(|_| make_event(300)).collect();
        let total_bytes = 1500;
        let (trimmed, bytes_removed) = trim_raw_log(&mut log, total_bytes, 1000, 10000);
        // Need to remove at least 500 bytes from front
        // 300 bytes per event: remove 2 events = 600 bytes removed, 900 remaining <= 1000
        assert_eq!(trimmed, 2);
        assert_eq!(bytes_removed, 600);
        assert_eq!(log.len(), 3);
    }

    #[test]
    fn test_trim_raw_log_over_event_limit() {
        // 10 events, max_events = 5
        let mut log: VecDeque<RawEvent> = (0..10).map(|_| make_event(10)).collect();
        let total_bytes = 100;
        let (trimmed, bytes_removed) = trim_raw_log(&mut log, total_bytes, 10_000_000, 5);
        assert_eq!(trimmed, 5);
        assert_eq!(bytes_removed, 50);
        assert_eq!(log.len(), 5);
    }

    #[test]
    fn test_trim_raw_log_both_limits() {
        // 20 events of 1000 bytes each = 20000 total
        // max_bytes = 5000, max_events = 8
        // Byte limit requires removing 15000 bytes = 15 events
        // Event limit requires removing 12 events
        // Byte limit is stricter, so 15 events removed
        let mut log: VecDeque<RawEvent> = (0..20).map(|_| make_event(1000)).collect();
        let total_bytes = 20000;
        let (trimmed, bytes_removed) = trim_raw_log(&mut log, total_bytes, 5000, 8);
        assert_eq!(trimmed, 15);
        assert_eq!(bytes_removed, 15000);
        assert_eq!(log.len(), 5);
    }

    #[test]
    fn test_trim_raw_log_exact_boundary() {
        // Exactly at the byte limit - should not trim
        let mut log = VecDeque::from(vec![make_event(500), make_event(500)]);
        let total_bytes = 1000;
        let (trimmed, bytes_removed) = trim_raw_log(&mut log, total_bytes, 1000, 10);
        assert_eq!(trimmed, 0);
        assert_eq!(bytes_removed, 0);
        assert_eq!(log.len(), 2);
    }

    #[test]
    fn test_trim_raw_log_exact_event_boundary() {
        // Exactly at the event limit - should not trim
        let mut log: VecDeque<RawEvent> = (0..5).map(|_| make_event(10)).collect();
        let total_bytes = 50;
        let (trimmed, bytes_removed) = trim_raw_log(&mut log, total_bytes, 10000, 5);
        assert_eq!(trimmed, 0);
        assert_eq!(bytes_removed, 0);
        assert_eq!(log.len(), 5);
    }

    #[test]
    fn test_trim_raw_log_one_over_byte_limit() {
        // 1 byte over the byte limit
        let mut log = VecDeque::from(vec![make_event(501), make_event(500)]);
        let total_bytes = 1001;
        let (trimmed, bytes_removed) = trim_raw_log(&mut log, total_bytes, 1000, 10000);
        // Remove first event (501 bytes), remaining = 500 <= 1000
        assert_eq!(trimmed, 1);
        assert_eq!(bytes_removed, 501);
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn test_trim_raw_log_single_huge_event() {
        // One event larger than the entire max_bytes budget
        let mut log = VecDeque::from(vec![make_event(5000)]);
        let total_bytes = 5000;
        // After trimming the one event, log is empty, 0 bytes remain which is <= 1000
        let (trimmed, bytes_removed) = trim_raw_log(&mut log, total_bytes, 1000, 10000);
        assert_eq!(trimmed, 1);
        assert_eq!(bytes_removed, 5000);
        assert_eq!(log.len(), 0);
    }

    #[test]
    fn test_trim_raw_log_empty() {
        let mut log: VecDeque<RawEvent> = VecDeque::new();
        let (trimmed, bytes_removed) = trim_raw_log(&mut log, 0, 1000, 10);
        assert_eq!(trimmed, 0);
        assert_eq!(bytes_removed, 0);
        assert_eq!(log.len(), 0);
    }

    #[test]
    fn test_trim_raw_log_preserves_newest() {
        // Events with ascending timestamps; oldest should be removed first
        let mut log = VecDeque::from(vec![
            make_event_with_ts(100, 1000),
            make_event_with_ts(100, 2000),
            make_event_with_ts(100, 3000),
            make_event_with_ts(100, 4000),
            make_event_with_ts(100, 5000),
        ]);
        let total_bytes = 500;
        // max_events = 3 means we need to remove 2 oldest
        let (trimmed, bytes_removed) = trim_raw_log(&mut log, total_bytes, 10000, 3);
        assert_eq!(trimmed, 2);
        assert_eq!(bytes_removed, 200);
        assert_eq!(log.len(), 3);
        // Remaining events should be the newest ones (timestamps 3000, 4000, 5000)
        assert_eq!(log[0].timestamp_us, 3000);
        assert_eq!(log[1].timestamp_us, 4000);
        assert_eq!(log[2].timestamp_us, 5000);
    }

    #[test]
    fn test_trim_raw_log_variable_event_sizes() {
        // Events with different sizes: [100, 200, 50, 300, 150] = 800 total
        // max_bytes = 500 => need to remove 300+ bytes from front
        let mut log = VecDeque::from(vec![
            make_event(100),
            make_event(200),
            make_event(50),
            make_event(300),
            make_event(150),
        ]);
        let total_bytes = 800;
        let (trimmed, bytes_removed) = trim_raw_log(&mut log, total_bytes, 500, 10000);
        // Remove event 0 (100): remaining 700 > 500
        // Remove event 1 (200): remaining 500 <= 500 - stop
        assert_eq!(trimmed, 2);
        assert_eq!(bytes_removed, 300);
        assert_eq!(log.len(), 3);
        // Remaining sizes: 50, 300, 150
        assert_eq!(log[0].byte_size(), 50);
        assert_eq!(log[1].byte_size(), 300);
        assert_eq!(log[2].byte_size(), 150);
    }

    #[test]
    fn test_trim_raw_log_returns_correct_counts() {
        let mut log: VecDeque<RawEvent> = (0..100).map(|_| make_event(50)).collect();
        let total_bytes = 5000;
        let (trimmed, bytes_removed) = trim_raw_log(&mut log, total_bytes, 1000, 10000);
        // Need to go from 5000 to <= 1000, removing 50 bytes per event
        // 80 events * 50 = 4000 removed, 1000 remaining
        assert_eq!(trimmed, 80);
        assert_eq!(bytes_removed, 4000);
        assert_eq!(log.len(), 20);
    }

    // --- trim_decoded_events tests ---

    #[test]
    fn test_trim_decoded_under_limit() {
        let mut events = VecDeque::from(vec![1, 2, 3]);
        trim_decoded_events(&mut events, 10);
        assert_eq!(events.len(), 3);
        assert_eq!(events, VecDeque::from(vec![1, 2, 3]));
    }

    #[test]
    fn test_trim_decoded_over_limit() {
        let mut events: VecDeque<i32> = (0..100).collect();
        trim_decoded_events(&mut events, 10);
        assert_eq!(events.len(), 10);
        // Should keep the last 10 (90..100)
        assert_eq!(events[0], 90);
        assert_eq!(events[9], 99);
    }

    #[test]
    fn test_trim_decoded_exact_limit() {
        let mut events: VecDeque<i32> = (0..5).collect();
        trim_decoded_events(&mut events, 5);
        assert_eq!(events.len(), 5);
        assert_eq!(events, VecDeque::from(vec![0, 1, 2, 3, 4]));
    }

    #[test]
    fn test_trim_decoded_preserves_newest() {
        // 8 events, keep max 3 => remove first 5, keep last 3
        let mut events = VecDeque::from(vec![10, 20, 30, 40, 50, 60, 70, 80]);
        trim_decoded_events(&mut events, 3);
        assert_eq!(events.len(), 3);
        assert_eq!(events, VecDeque::from(vec![60, 70, 80]));
    }

    #[test]
    fn test_trim_decoded_empty() {
        let mut events: VecDeque<i32> = VecDeque::new();
        trim_decoded_events(&mut events, 10);
        assert_eq!(events.len(), 0);
    }

    #[test]
    fn test_trim_decoded_one_over() {
        let mut events = VecDeque::from(vec![1, 2, 3, 4]);
        trim_decoded_events(&mut events, 3);
        assert_eq!(events.len(), 3);
        assert_eq!(events, VecDeque::from(vec![2, 3, 4]));
    }

    #[test]
    fn test_trim_decoded_max_one() {
        let mut events = VecDeque::from(vec![10, 20, 30]);
        trim_decoded_events(&mut events, 1);
        assert_eq!(events.len(), 1);
        assert_eq!(events, VecDeque::from(vec![30]));
    }

    // --- Integration-style tests using production constants ---

    #[test]
    fn test_trim_raw_log_with_production_constants() {
        // Simulate exceeding MAX_LOG_EVENTS with small events
        let count = MAX_LOG_EVENTS + 100;
        let mut log: VecDeque<RawEvent> = (0..count)
            .map(|i| make_event_with_ts(10, i as u64))
            .collect();
        let total_bytes = count * 10;
        let (trimmed, bytes_removed) =
            trim_raw_log(&mut log, total_bytes, MAX_LOG_BYTES, MAX_LOG_EVENTS);
        assert_eq!(trimmed, 100);
        assert_eq!(bytes_removed, 1000);
        assert_eq!(log.len(), MAX_LOG_EVENTS);
        // First remaining event should have timestamp 100
        assert_eq!(log[0].timestamp_us, 100);
    }

    #[test]
    fn test_trim_decoded_with_production_constants() {
        let count = MAX_DECODED_EVENTS + 50;
        let mut events: VecDeque<u32> = (0..count as u32).collect();
        trim_decoded_events(&mut events, MAX_DECODED_EVENTS);
        assert_eq!(events.len(), MAX_DECODED_EVENTS);
        assert_eq!(events[0], 50);
    }
}
