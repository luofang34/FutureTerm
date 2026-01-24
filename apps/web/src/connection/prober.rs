use super::types::parse_framing;
use core_types::SerialConfig;
use core_types::Transport;
use futures::future::{select, Either};
use leptos::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;
use transport_webserial::WebSerialTransport;

pub async fn detect_config(
    port: web_sys::SerialPort,
    current_framing: &str,
    set_status: WriteSignal<String>,
    probing_interrupted: Rc<Cell<bool>>,
    last_auto_baud: Rc<RefCell<Option<u32>>>,
) -> (u32, String, Vec<u8>, Option<String>) {
    // State machine already in Probing state (set by caller)
    // ondisconnect handler will ignore disconnects during Probing

    let baud_candidates = vec![
        115200, 1500000, 1000000, 2000000, 921600, 57600, 460800, 230400, 38400, 19200, 9600,
    ];
    let mut best_score = 0.0;
    let mut best_rate = 115200;
    let mut best_framing = "8N1".to_string();
    let mut best_buffer = Vec::new();
    let mut best_proto = None;

    'outer: for rate in baud_candidates {
        // OPTIMIZATION: Abort probing early if disconnect detected
        if probing_interrupted.get() {
            #[cfg(debug_assertions)]
            web_sys::console::log_1(
                &"DEBUG: Probing aborted - disconnect detected, remaining probes skipped".into(),
            );
            break 'outer;
        }

        set_status.set(format!("Scanning {}...", rate));
        #[cfg(debug_assertions)]
        web_sys::console::log_1(&format!("AUTO: Probing [v2] {}...", rate).into());
        let probe_start_ts = js_sys::Date::now();

        // 1. Probe 8N1
        let buffer = gather_probe_data(port.clone(), rate, "8N1", true).await;
        let open_dur = js_sys::Date::now() - probe_start_ts;
        #[cfg(debug_assertions)]
        web_sys::console::log_1(
            &format!(
                "PROFILE: Rate {} PROBED in {:.1}ms. Bytes: {}",
                rate,
                open_dur,
                buffer.len()
            )
            .into(),
        );

        // Check if user disconnected while we were probing this rate
        if probing_interrupted.get() {
            #[cfg(debug_assertions)]
            web_sys::console::log_1(
                &"DEBUG: Probing aborted - disconnect detected after probe completed".into(),
            );
            break 'outer;
        }

        if buffer.is_empty() {
            continue;
        }

        // 2. Analyze
        let score_8n1 = analysis::calculate_score_8n1(&buffer);
        let score_7e1 = analysis::calculate_score_7e1(&buffer);
        let score_mav = analysis::calculate_score_mavlink(&buffer);

        #[cfg(debug_assertions)]
        web_sys::console::log_1(
            &format!(
                "AUTO: Rate {} => 8N1: {:.4}, 7E1: {:.4}, MAV: {:.4} (Size: {})",
                rate,
                score_8n1,
                score_7e1,
                score_mav,
                buffer.len()
            )
            .into(),
        );

        // MAVLink Priority Check (Robust)
        #[cfg(feature = "mavlink")]
        if verify_mavlink_integrity(&buffer) {
            best_rate = rate;
            best_framing = "8N1".to_string(); // MAVLink is 8N1
            best_buffer = buffer.clone();
            best_proto = Some("mavlink".to_string());
            #[cfg(debug_assertions)]
            web_sys::console::log_1(
                &"AUTO: MAVLink Verified (Magic+Parse)! Stopping probe.".into(),
            );
            break 'outer;
        }

        // Fallback to statistical score if verification inconclusive but score high
        if score_mav >= 0.99 {
            best_rate = rate;
            best_framing = "8N1".to_string(); // MAVLink is 8N1
            best_buffer = buffer.clone();
            best_proto = Some("mavlink".to_string());
            web_sys::console::log_1(
                &"AUTO: MAVLink Detected (Statistical). Stopping probe.".into(),
            );
            break 'outer;
        }

        if score_8n1 > best_score {
            best_score = score_8n1;
            best_rate = rate;
            best_framing = "8N1".to_string();
            best_buffer = buffer.clone();
        }
        if score_7e1 > best_score {
            best_score = score_7e1;
            best_rate = rate;
            best_framing = "7E1".to_string();
            best_buffer = buffer.clone();
        }

        // Optimization: If "Perfect" match found, stop scanning remaining rates
        if best_score > 0.99 && best_buffer.len() > 64 {
            web_sys::console::log_1(
                &format!("AUTO: Perfect match found at {}. Stopping scan.", best_rate).into(),
            );
            *last_auto_baud.borrow_mut() = Some(best_rate); // SAVE CACHE
            break 'outer;
        }

        // High-Speed Optimization: Accept lower confidence for >= 1M baud
        let threshold = if best_rate >= 1000000 { 0.85 } else { 0.98 };

        if best_score > threshold {
            web_sys::console::log_1(
                &format!(
                    "AUTO: Early Break at {} (Score: {:.2} > {})",
                    best_rate, best_score, threshold
                )
                .into(),
            );
            *last_auto_baud.borrow_mut() = Some(best_rate); // SAVE CACHE
            break 'outer;
        }

        // 3. Fallback: Deep Probe if Auto Framing
        if current_framing == "Auto" && best_score < 0.5 {
            for fr in ["8E1", "8O1"] {
                set_status.set(format!("Deep Probe {} {}...", rate, fr));

                let buf2 = gather_probe_data(port.clone(), rate, fr, true).await;
                let score = analysis::calculate_score_8n1(&buf2);
                if score > best_score {
                    best_score = score;
                    best_rate = rate;
                    best_framing = fr.to_string();
                    best_buffer = buf2;
                }
                if score > 0.95 {
                    break 'outer;
                }
            }
        }
    }

    // Status will be updated by FSM state transition in caller

    (best_rate, best_framing, best_buffer, best_proto)
}

// Helper: Gather Probe Data
async fn gather_probe_data(
    port: web_sys::SerialPort,
    rate: u32,
    framing: &str,
    send_wakeup: bool,
) -> Vec<u8> {
    let mut t = WebSerialTransport::new();
    let (d, p, s) = parse_framing(framing);
    let cfg = SerialConfig {
        baud_rate: rate,
        data_bits: d,
        parity: p,
        stop_bits: s,
        flow_control: "none".into(),
    };

    let mut buffer = Vec::new();
    let mut attempts = 0;
    let success = loop {
        match t.open(port.clone(), cfg.clone()).await {
            Ok(_) => break true,
            Err(e) => {
                let err_str = format!("{:?}", e);
                // NOTE: Error string matching is fragile and may break with library updates.
                // Ideally we'd match on error types, but WebSerial errors are opaque JsValue.
                // These specific strings have been stable across WebSerial implementations.
                if (err_str.contains("already open") || err_str.contains("InvalidStateError"))
                    && attempts < 3
                {
                    attempts += 1;
                    let _ =
                        wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(&mut |r, _| {
                            if let Some(window) = web_sys::window() {
                                let _ = window
                                    .set_timeout_with_callback_and_timeout_and_arguments_0(&r, 200);
                            }
                        }))
                        .await;
                    continue;
                }
                break false;
            }
        }
    };

    if success {
        if send_wakeup {
            let _ = t.write(b"\r").await;
        }

        let start = js_sys::Date::now();
        let mut max_time = 150.0;

        loop {
            let elapsed = js_sys::Date::now() - start;
            if elapsed > max_time {
                break;
            }

            let remaining = (max_time - elapsed).max(10.0) as i32;

            // Timeout Future
            let timeout_fut = async {
                let promise = js_sys::Promise::new(&mut |r, _| {
                    if let Some(window) = web_sys::window() {
                        let _ = window
                            .set_timeout_with_callback_and_timeout_and_arguments_0(&r, remaining);
                    }
                });
                let _ = wasm_bindgen_futures::JsFuture::from(promise).await;
                None
            };

            // Read Future
            let read_fut = async { t.read_chunk().await.ok() };

            // Race: Read vs Timeout
            // We use Box::pin to ensure safety with select
            let result = match select(Box::pin(read_fut), Box::pin(timeout_fut)).await {
                Either::Left((res, _)) => res,
                Either::Right((res, _)) => res,
            };

            match result {
                Some((bytes, _ts)) => {
                    if !bytes.is_empty() {
                        buffer.extend_from_slice(&bytes);

                        // Adaptive strategy: If we find data, extend the probe window slightly
                        // to capture the full message, but don't hang forever.
                        if buffer.len() > 200 {
                            break; // Enough data
                        }
                        max_time = 250.0;
                    }
                }
                None => {
                    // Timeout occurred
                    break;
                }
            }

            // Extra safety exit
            if buffer.len() > 64 && analysis::calculate_score_8n1(&buffer) > 0.90 {
                break;
            }
        }

        let _ = t.close().await;

        // Mandatory Cool-down
        let _ = wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(&mut |r, _| {
            if let Some(window) = web_sys::window() {
                let _ = window.set_timeout_with_callback_and_timeout_and_arguments_0(&r, 200);
            }
        }))
        .await;
    }
    buffer
}

#[cfg(feature = "mavlink")]
fn verify_mavlink_integrity(buffer: &[u8]) -> bool {
    let mut reader = buffer;
    loop {
        let magic_idx = reader.iter().position(|&b| b == 0xFE || b == 0xFD);
        if let Some(idx) = magic_idx {
            if idx + 1 >= reader.len() {
                return false;
            }
            let Some(&magic) = reader.get(idx) else {
                return false;
            };
            let min_packet_size = if magic == 0xFE { 8 } else { 12 };

            if idx + min_packet_size > reader.len() {
                return false;
            }

            let Some(sub_slice) = reader.get(idx..) else {
                return false;
            };

            let mut try_reader = sub_slice;
            let res = if magic == 0xFE {
                mavlink::read_v1_msg::<mavlink::common::MavMessage, _>(&mut try_reader)
            } else {
                mavlink::read_v2_msg::<mavlink::common::MavMessage, _>(&mut try_reader)
            };

            if res.is_ok() {
                log::debug!("MAVLink VERIFIED. Magic: {:02X}", magic);
                return true;
            } else {
                log::debug!("MAVLink Magic found but parse failed ({:?}).", res.err());
            }

            let Some(next_reader) = reader.get(idx + 1..) else {
                return false;
            };
            reader = next_reader;
        } else {
            return false;
        }
    }
}

pub async fn smart_probe_framing(
    port: web_sys::SerialPort,
    rate: u32,
    set_status: WriteSignal<String>,
) -> (String, Vec<u8>, Option<String>) {
    set_status.set(format!("Smart Probing {}...", rate));

    // 1. Probe 8N1
    let buf_8n1 = gather_probe_data(port.clone(), rate, "8N1", true).await;
    if !buf_8n1.is_empty() {
        let score = analysis::calculate_score_8n1(&buf_8n1);
        let score_mav = analysis::calculate_score_mavlink(&buf_8n1);

        if score_mav >= 0.99 {
            return ("8N1".to_string(), buf_8n1, Some("mavlink".to_string()));
        }

        if score > 0.90 {
            return ("8N1".to_string(), buf_8n1, None);
        }
    }

    // 2. Probe 7E1
    if !buf_8n1.is_empty() {
        let buf_7e1 = gather_probe_data(port.clone(), rate, "7E1", true).await;
        let score = analysis::calculate_score_7e1(&buf_7e1);
        if score > 0.90 {
            return ("7E1".to_string(), buf_7e1, None);
        }
    }

    ("8N1".to_string(), buf_8n1, None)
}

#[cfg(test)]
#[cfg(feature = "mavlink")]
mod tests {
    use super::*;

    #[test]
    fn test_mavlink_verification_valid_v1() {
        // MAVLink v1 HEARTBEAT (fe 09 4e 01 01 00 00 00 00 00 02 03 51 04 03 1c 7f)
        let msg = vec![
            0xfe, 0x09, 0x4e, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x03, 0x51, 0x04,
            0x03, 0x1c, 0x7f,
        ];
        assert!(verify_mavlink_integrity(&msg));
    }

    #[test]
    fn test_mavlink_verification_invalid_magic() {
        let msg = vec![
            0xff, 0x09, 0x4e, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x03, 0x51, 0x04,
            0x03, 0x1c, 0x7f,
        ];
        assert!(!verify_mavlink_integrity(&msg));
    }

    #[test]
    fn test_mavlink_verification_too_short() {
        let msg = vec![0xfe, 0x09, 0x4e];
        assert!(!verify_mavlink_integrity(&msg));
    }
}
