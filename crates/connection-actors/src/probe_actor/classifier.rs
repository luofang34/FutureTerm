use actor_protocol::ProbeResult;

#[cfg(feature = "mavlink")]
use mavlink;

impl super::ProbeActor {
    pub(super) fn analyze_buffer(&self, buffer: &[u8], baud: u32) -> ProbeResult {
        if buffer.is_empty() {
            return ProbeResult {
                baud,
                framing: "8N1".into(),
                protocol: None,
                initial_data: buffer.to_vec(),
                confidence: 0.0,
            };
        }

        // Use proper scoring functions from analysis crate
        let score_8n1 = analysis::calculate_score_8n1(buffer) as f64;
        let score_7e1 = analysis::calculate_score_7e1(buffer) as f64;
        let score_mav = analysis::calculate_score_mavlink(buffer) as f64;

        // Debug log scoring for development
        #[cfg(debug_assertions)]
        #[cfg(target_arch = "wasm32")]
        web_sys::console::log_1(
            &format!(
                "AUTO: Rate {} => 8N1: {:.4}, 7E1: {:.4}, MAV: {:.4} (Size: {})",
                baud,
                score_8n1,
                score_7e1,
                score_mav,
                buffer.len()
            )
            .into(),
        );

        // MAVLink Priority Check (Robust) - Use integrity verification if available
        #[cfg(feature = "mavlink")]
        if self.verify_mavlink_integrity(buffer) {
            #[cfg(debug_assertions)]
            #[cfg(target_arch = "wasm32")]
            web_sys::console::log_1(&"AUTO: MAVLink Verified (Magic+Parse)!".into());

            return ProbeResult {
                baud,
                framing: "8N1".into(),
                protocol: Some("mavlink".into()),
                initial_data: buffer.to_vec(),
                confidence: 1.0,
            };
        }

        // Fallback to statistical score if verification inconclusive but score high
        if score_mav >= 0.99 {
            #[cfg(debug_assertions)]
            #[cfg(target_arch = "wasm32")]
            web_sys::console::log_1(&"AUTO: MAVLink Detected (Statistical).".into());

            return ProbeResult {
                baud,
                framing: "8N1".into(),
                protocol: Some("mavlink".into()),
                initial_data: buffer.to_vec(),
                confidence: score_mav,
            };
        }

        // Check for NMEA signature ($GP, $GN, etc.) - simple pattern detection
        let has_nmea = buffer.windows(2).any(|w| matches!(w, [b'$', b'G']));
        if has_nmea && score_8n1 > 0.85 {
            return ProbeResult {
                baud,
                framing: "8N1".into(),
                protocol: Some("nmea".into()),
                initial_data: buffer.to_vec(),
                confidence: score_8n1,
            };
        }

        // Find best score among framing options
        let mut best_score = score_8n1;
        let mut best_framing = "8N1";

        if score_7e1 > best_score {
            best_score = score_7e1;
            best_framing = "7E1";
        }

        // Check for early break threshold
        // High-Speed Optimization: Accept lower confidence for >= 1M baud
        let threshold = if baud >= 1_000_000 { 0.85 } else { 0.98 };

        if best_score > threshold && buffer.len() > 64 {
            #[cfg(debug_assertions)]
            #[cfg(target_arch = "wasm32")]
            web_sys::console::log_1(
                &format!(
                    "AUTO: High confidence match at {} baud (Score: {:.2} > {})",
                    baud, best_score, threshold
                )
                .into(),
            );
        }

        ProbeResult {
            baud,
            framing: best_framing.into(),
            protocol: None,
            initial_data: buffer.to_vec(),
            confidence: best_score,
        }
    }

    #[cfg(feature = "mavlink")]
    pub(super) fn verify_mavlink_integrity(&self, buffer: &[u8]) -> bool {
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

                // Wrap in PeekReader as required by MAVLink API
                // &[u8] implements embedded_io::Read, so we can use it directly
                let mut peek_reader = mavlink::peek_reader::PeekReader::new(sub_slice);

                let res = if magic == 0xFE {
                    mavlink::read_v1_msg::<mavlink::common::MavMessage, _>(&mut peek_reader)
                } else {
                    mavlink::read_v2_msg::<mavlink::common::MavMessage, _>(&mut peek_reader)
                };

                if res.is_ok() {
                    #[cfg(debug_assertions)]
                    #[cfg(target_arch = "wasm32")]
                    web_sys::console::log_1(
                        &format!("MAVLink VERIFIED. Magic: {:02X}", magic).into(),
                    );
                    return true;
                } else {
                    #[cfg(debug_assertions)]
                    #[cfg(target_arch = "wasm32")]
                    web_sys::console::log_1(&"MAVLink Magic found but parse failed.".into());
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
}
