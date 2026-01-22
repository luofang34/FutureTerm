use core_types::{DecodedEvent, Decoder, Frame};
use mavlink::{MavHeader, Message};
// Use std::io traits

const MAX_BUFFER_SIZE: usize = 1024;
const INITIAL_BUFFER_CAPACITY: usize = 1024;
const GARBAGE_SKIP_THRESHOLD: usize = 200;
const MAVLINK_V1_MAGIC: u8 = 0xFE;
const MAVLINK_V2_MAGIC: u8 = 0xFD;
const MAVLINK_V2_SIGNATURE_SIZE: usize = 13;

pub struct MavlinkDecoder {
    buffer: Vec<u8>,
}

impl MavlinkDecoder {
    pub fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(INITIAL_BUFFER_CAPACITY),
        }
    }

    fn parse_message(
        &self,
        header: &MavHeader,
        body: &mavlink::common::MavMessage,
    ) -> DecodedEvent {
        let msg_name = body.message_name();
        let mut event = DecodedEvent::new(0, "MAVLink", msg_name); // Timestamp set later

        event.confidence = 1.0;

        // Version is implicit from parser used, but we don't pass it in `header`.
        // We can't easily set it here without passing it from ingest loop.
        // But for now, let's just default to unknown or remove the field?
        // Or better: The `msg` itself sometimes knows? No.
        // Let's just hardcode "MAVLink" protocol name.

        event = event.with_field("sys_id", header.system_id as i64);
        event = event.with_field("comp_id", header.component_id as i64);
        event = event.with_field("seq", header.sequence as i64);

        match body {
            mavlink::common::MavMessage::HEARTBEAT(msg) => {
                event = event.with_field("type", format!("{:?}", msg.mavtype));
                event = event.with_field("autopilot", format!("{:?}", msg.autopilot));
                event = event.with_field("base_mode", format!("{:?}", msg.base_mode));
                event = event.with_field("system_status", format!("{:?}", msg.system_status));
                event = event.with_field("mavlink_version", msg.mavlink_version as i64);
            }
            mavlink::common::MavMessage::ATTITUDE(msg) => {
                event = event.with_field("roll", msg.roll);
                event = event.with_field("pitch", msg.pitch);
                event = event.with_field("yaw", msg.yaw);
            }
            mavlink::common::MavMessage::GLOBAL_POSITION_INT(msg) => {
                event = event.with_field("lat", msg.lat as f64 / 1e7);
                event = event.with_field("lon", msg.lon as f64 / 1e7);
                event = event.with_field("alt", msg.alt as f64 / 1000.0);
                event = event.with_field("relative_alt", msg.relative_alt as f64 / 1000.0);
            }
            mavlink::common::MavMessage::GPS_RAW_INT(msg) => {
                event = event.with_field("fix_type", format!("{:?}", msg.fix_type));
                event = event.with_field("lat", msg.lat as f64 / 1e7);
                event = event.with_field("lon", msg.lon as f64 / 1e7);
                event = event.with_field("alt", msg.alt as f64 / 1000.0);
                event = event.with_field("satellites_visible", msg.satellites_visible as i64);
            }
            mavlink::common::MavMessage::SYS_STATUS(msg) => {
                event = event.with_field("voltage_battery", msg.voltage_battery as f64 / 1000.0);
                event = event.with_field("current_battery", msg.current_battery as i64);
                event = event.with_field("battery_remaining", msg.battery_remaining as i64);
                event = event.with_field("load", msg.load as f64 / 10.0);
            }
            _ => {
                // Generic fallback for other messages?
            }
        }
        event
    }
}

impl Default for MavlinkDecoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder for MavlinkDecoder {
    fn ingest(&mut self, frame: &Frame, results: &mut Vec<DecodedEvent>) {
        self.buffer.extend_from_slice(&frame.bytes);

        // Safety Cap
        if self.buffer.len() > MAX_BUFFER_SIZE {
            #[cfg(target_arch = "wasm32")]
            web_sys::console::warn_1(&"MAVLink buffer exceeded limit, clearing".into());
            self.buffer.clear();
        }

        // Process buffer
        loop {
            // Minimal length check (header)
            // Minimal length check (header)
            // Check for minimum v2 size (more conservative)
            if self.buffer.len() < 12 {
                break;
            }

            // Look for magic byte
            // v1 (0xFE) or v2 (0xFD)
            let magic_idx = self
                .buffer
                .iter()
                .position(|&b| b == MAVLINK_V1_MAGIC || b == MAVLINK_V2_MAGIC);

            if let Some(idx) = magic_idx {
                // Discard garbage before magic byte
                if idx > 0 {
                    // leptos::logging::log!("MAVLink: Garbage skipped: {} bytes", idx);
                    self.buffer.drain(0..idx);
                }
            } else {
                if self.buffer.len() > GARBAGE_SKIP_THRESHOLD {
                    // leptos::logging::log!("MAVLink: Buffer full ({}) no magic found, clearing",
                    // self.buffer.len());
                    self.buffer.clear();
                }
                break;
            }

            let Some(&magic) = self.buffer.first() else {
                break;
            };
            let Some(&payload_len_byte) = self.buffer.get(1) else {
                break;
            };
            let payload_len = payload_len_byte as usize;

            let base_len = if magic == MAVLINK_V1_MAGIC {
                // v1: header(6) + payload + crc(2)
                8 + payload_len
            } else {
                // v2: header(10) + payload + crc(2)
                12 + payload_len
            };

            let mut total_len = base_len;
            if magic == MAVLINK_V2_MAGIC {
                if let Some(&incompat_flags) = self.buffer.get(2) {
                    if incompat_flags & 0x01 != 0 {
                        total_len += MAVLINK_V2_SIGNATURE_SIZE; // MAVLink v2 signature
                    }
                }
            }

            if self.buffer.len() < total_len {
                break; // Wait for more data
            }

            // Try parse using the wrapper
            // Try parse using slice reader (requires std::io::Read for &[u8])
            let Some(reader_slice) = self.buffer.get(0..total_len) else {
                break;
            };
            let mut reader = reader_slice;

            let parse_result = if magic == MAVLINK_V1_MAGIC {
                mavlink::read_v1_msg::<mavlink::common::MavMessage, _>(&mut reader)
            } else {
                mavlink::read_v2_msg::<mavlink::common::MavMessage, _>(&mut reader)
            };

            match parse_result {
                Ok((header, body)) => {
                    let mut event = self.parse_message(&header, &body);
                    event.timestamp_us = frame.timestamp_us; // Inherit timestamp
                    let ver_str = if magic == MAVLINK_V1_MAGIC {
                        "v1"
                    } else {
                        "v2"
                    };
                    event = event.with_field("version", ver_str.to_string());
                    results.push(event);

                    // Remove consumed bytes
                    self.buffer.drain(0..total_len);
                }
                Err(_e) => {
                    // CRC fail or parse error?
                    // Log the error for visibility
                    #[cfg(target_arch = "wasm32")]
                    web_sys::console::log_1(&format!("MAVLink parse failed: {:?}", _e).into());

                    // Advance 1 byte to try resync
                    self.buffer.drain(0..1);
                }
            }
        }
    }

    fn id(&self) -> &'static str {
        "mavlink"
    }

    fn name(&self) -> &'static str {
        "MAVLink v1/v2"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_mavlink_heartbeat_parsing() {
        let mut decoder = MavlinkDecoder::new();
        // Manual construction of a packet byte buffer to avoid dependency on crate writing (which
        // might need std::io::Write) Or we use the crate?
        // With embedded feature, write_v1_msg takes embedded::Write.
        // We can implement a ByteVecWriter.

        // pre-canned heartbeat v1 for Ardupilot
        // FE 09 00 01 01 00 00 00 00 00 02 03 51 04 03 1C 7F (CRC is example)
        // Let's rely on garbage handling test mostly, or try to init from struct if possible
        // without std write.

        // Actually, we can use a known valid packet from online example or construct one.
        // With std feature, write_v1_msg takes std::io::Write.
        // Vec<u8> implements Write.

        let header = mavlink::MavHeader {
            system_id: 1,
            component_id: 1,
            sequence: 42,
        };
        let msg = mavlink::common::MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA {
            custom_mode: 0,
            mavtype: mavlink::common::MavType::MAV_TYPE_QUADROTOR,
            autopilot: mavlink::common::MavAutopilot::MAV_AUTOPILOT_ARDUPILOTMEGA,
            base_mode: mavlink::common::MavModeFlag::MAV_MODE_FLAG_MANUAL_INPUT_ENABLED,
            system_status: mavlink::common::MavState::MAV_STATE_ACTIVE,
            mavlink_version: 3,
        });

        let mut writer = Vec::new();
        mavlink::write_v1_msg(&mut writer, header, &msg).expect("Write failed");

        let frame = Frame::new_rx(writer, 1000);
        let mut results = Vec::new();
        decoder.ingest(&frame, &mut results);

        assert!(!results.is_empty());
        let event = &results[0];
        assert_eq!(event.summary, "HEARTBEAT");
    }

    #[test]
    fn test_mavlink_garbage_skipping() {
        let mut decoder = MavlinkDecoder::new();

        let header = mavlink::MavHeader {
            system_id: 1,
            component_id: 1,
            sequence: 42,
        };
        let msg = mavlink::common::MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA {
            custom_mode: 0,
            mavtype: mavlink::common::MavType::MAV_TYPE_QUADROTOR,
            autopilot: mavlink::common::MavAutopilot::MAV_AUTOPILOT_ARDUPILOTMEGA,
            base_mode: mavlink::common::MavModeFlag::MAV_MODE_FLAG_MANUAL_INPUT_ENABLED,
            system_status: mavlink::common::MavState::MAV_STATE_ACTIVE,
            mavlink_version: 3,
        });

        let mut writer = Vec::new();
        mavlink::write_v1_msg(&mut writer, header, &msg).expect("Write failed");

        // Prepend garbage
        let mut data = vec![0x00, 0xFF, 0xAA, 0x55]; // Random junk
        data.extend_from_slice(&writer);
        data.push(0xFF); // Trailing junk

        let frame = Frame::new_rx(data, 2000);
        let mut results = Vec::new();

        // First ingest (should find packet after garbage)
        decoder.ingest(&frame, &mut results);

        assert_eq!(
            results.len(),
            1,
            "Should have found 1 packet despite garbage"
        );
        assert_eq!(results[0].summary, "HEARTBEAT");

        // Verify buffer is clean (trailing junk might remain or be consumed if not enough for
        // header) logic: invalid magic -> drain 1. 0xFF is invalid magic start (if not
        // FE/FD). 0xFF != FE/FD. Drains. Buffer empty.
    }

    #[test]
    fn test_buffer_overflow_protection() {
        let mut decoder = MavlinkDecoder::new();
        // Fill buffer with junk beyond limit
        let huge_data = vec![0xFF; MAX_BUFFER_SIZE + 50]; // No valid magic
        let frame = Frame::new_rx(huge_data, 3000);
        let mut results = Vec::new();

        decoder.ingest(&frame, &mut results);

        // Should have cleared buffer
        assert!(
            decoder.buffer.len() < MAX_BUFFER_SIZE,
            "Buffer did not clear on overflow"
        );
        assert!(
            decoder.buffer.is_empty(),
            "Buffer should be empty after clear"
        );
    }

    #[test]
    fn test_multi_packet_frame() {
        let mut decoder = MavlinkDecoder::new();

        // Create two identical valid packets
        let header = mavlink::MavHeader {
            system_id: 1,
            component_id: 1,
            sequence: 10,
        };
        let msg = mavlink::common::MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA {
            custom_mode: 0,
            mavtype: mavlink::common::MavType::MAV_TYPE_GENERIC,
            autopilot: mavlink::common::MavAutopilot::MAV_AUTOPILOT_GENERIC,
            base_mode: mavlink::common::MavModeFlag::MAV_MODE_FLAG_MANUAL_INPUT_ENABLED,
            system_status: mavlink::common::MavState::MAV_STATE_STANDBY,
            mavlink_version: 3,
        });

        let mut writer = Vec::new();
        mavlink::write_v1_msg(&mut writer, header.clone(), &msg).expect("Write 1 failed");
        let mut second_packet = Vec::new();
        let header2 = mavlink::MavHeader {
            system_id: 1,
            component_id: 1,
            sequence: 11,
        };
        mavlink::write_v1_msg(&mut second_packet, header2, &msg).expect("Write 2 failed");

        writer.extend_from_slice(&second_packet);

        let frame = Frame::new_rx(writer, 4000);
        let mut results = Vec::new();

        decoder.ingest(&frame, &mut results);

        assert_eq!(results.len(), 2, "Should parse two packets");
        let seq1 = results[0]
            .fields
            .iter()
            .find(|(k, _)| k == "seq")
            .unwrap()
            .1
            .clone();
        let seq2 = results[1]
            .fields
            .iter()
            .find(|(k, _)| k == "seq")
            .unwrap()
            .1
            .clone();
        assert_eq!(seq1, core_types::Value::I64(10));
        assert_eq!(seq2, core_types::Value::I64(11));
    }

    #[test]
    fn test_partial_packet_reception() {
        let mut decoder = MavlinkDecoder::new();

        // One valid packet
        let header = mavlink::MavHeader {
            system_id: 1,
            component_id: 1,
            sequence: 55,
        };
        let msg = mavlink::common::MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA {
            custom_mode: 0,
            mavtype: mavlink::common::MavType::MAV_TYPE_GENERIC,
            autopilot: mavlink::common::MavAutopilot::MAV_AUTOPILOT_GENERIC,
            base_mode: mavlink::common::MavModeFlag::MAV_MODE_FLAG_MANUAL_INPUT_ENABLED,
            system_status: mavlink::common::MavState::MAV_STATE_STANDBY,
            mavlink_version: 3,
        });

        let mut packet_data = Vec::new();
        mavlink::write_v1_msg(&mut packet_data, header, &msg).expect("Write failed");

        // Split into two frames
        let split_point = packet_data.len() / 2;
        let (first_half, second_half) = packet_data.split_at(split_point);

        let mut results = Vec::new();

        // 1. Ingest first half
        let frame1 = Frame::new_rx(first_half.to_vec(), 5000);
        decoder.ingest(&frame1, &mut results);
        assert!(results.is_empty(), "Should not parse partial packet");
        assert_eq!(
            decoder.buffer.len(),
            first_half.len(),
            "First half should be buffered"
        );

        // 2. Ingest second half
        let frame2 = Frame::new_rx(second_half.to_vec(), 5001);
        decoder.ingest(&frame2, &mut results);

        assert_eq!(results.len(), 1, "Should parse reassembled packet");
        let seq = results[0]
            .fields
            .iter()
            .find(|(k, _)| k == "seq")
            .unwrap()
            .1
            .clone();
        assert_eq!(seq, core_types::Value::I64(55));
    }

    fn calculate_crc(data: &[u8]) -> u16 {
        let mut crc: u16 = 0xffff;
        for byte in data {
            let mut tmp = *byte as u16 ^ (crc & 0xff);
            tmp = (tmp ^ (tmp << 4)) & 0xff;
            crc = (crc >> 8) ^ (tmp << 8) ^ (tmp << 3) ^ (tmp >> 4);
        }
        crc
    }

    #[test]
    fn test_mavlink_v2_with_signature_manual_crc() {
        let mut decoder = MavlinkDecoder::new();

        // Construct v2 packet
        let header = mavlink::MavHeader {
            system_id: 1,
            component_id: 1,
            sequence: 99,
        };
        let msg = mavlink::common::MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA {
            custom_mode: 0,
            mavtype: mavlink::common::MavType::MAV_TYPE_GENERIC,
            autopilot: mavlink::common::MavAutopilot::MAV_AUTOPILOT_GENERIC,
            base_mode: mavlink::common::MavModeFlag::MAV_MODE_FLAG_MANUAL_INPUT_ENABLED,
            system_status: mavlink::common::MavState::MAV_STATE_STANDBY,
            mavlink_version: 3,
        });

        let mut writer = Vec::new();
        mavlink::write_v2_msg(&mut writer, header, &msg).expect("Write v2 failed");

        // Set INCOMPAT_FLAGS_SIGNED (byte 2)
        writer[2] |= 0x01;

        // Recalculate CRC
        // V2 CRC excludes magic (byte 0).
        // Range: 1..writer.len()-2 (exclude existing CRC)
        // Add CRC_EXTRA for HEARTBEAT (50)
        let mut crc_data = writer[1..writer.len() - 2].to_vec();
        crc_data.push(50); // CRC_EXTRA for HEARTBEAT

        let new_crc = calculate_crc(&crc_data);
        let len = writer.len();
        writer[len - 2] = (new_crc & 0xFF) as u8;
        writer[len - 1] = ((new_crc >> 8) & 0xFF) as u8;

        // Append 13 dummy signature bytes
        writer.extend_from_slice(&[0x00; 13]);

        let frame = Frame::new_rx(writer, 6000);
        let mut results = Vec::new();

        decoder.ingest(&frame, &mut results);

        assert_eq!(results.len(), 1, "Should parse signed v2 packet");
        let version = results[0]
            .fields
            .iter()
            .find(|(k, _)| k == "version")
            .unwrap()
            .1
            .clone();
        assert_eq!(version, core_types::Value::String("v2".to_string()));
    }

    #[test]
    fn test_high_frequency_stream_stress() {
        let mut decoder = MavlinkDecoder::new();
        // Generate a single valid packet
        let header = mavlink::MavHeader {
            system_id: 1,
            component_id: 1,
            sequence: 0,
        };
        let msg = mavlink::common::MavMessage::HEARTBEAT(mavlink::common::HEARTBEAT_DATA {
            custom_mode: 0,
            mavtype: mavlink::common::MavType::MAV_TYPE_GENERIC,
            autopilot: mavlink::common::MavAutopilot::MAV_AUTOPILOT_GENERIC,
            base_mode: mavlink::common::MavModeFlag::MAV_MODE_FLAG_MANUAL_INPUT_ENABLED,
            system_status: mavlink::common::MavState::MAV_STATE_STANDBY,
            mavlink_version: 3,
        });

        let mut packet = Vec::new();
        mavlink::write_v1_msg(&mut packet, header, &msg).expect("Write failed");

        // Create a massive buffer with 10,000 packets
        // Note: 10,000 * ~12 bytes = ~120KB. MAX_BUFFER_SIZE is 1024.
        // If we ingest it all at once, it will hit the safety cap and clear!
        // The stress test should simulate *streaming* so we ingest in chunks.

        let count = 10_000;
        let mut big_stream = Vec::with_capacity(packet.len() * count);
        for _ in 0..count {
            big_stream.extend_from_slice(&packet);
        }

        let mut results = Vec::new();

        // Chunk size must be < MAX_BUFFER_SIZE to avoid safety cap
        // 1024 is the cap. Let's use 512 byte chunks.
        for chunk in big_stream.chunks(512) {
            let frame = Frame::new_rx(chunk.to_vec(), 0);
            decoder.ingest(&frame, &mut results);
        }

        assert_eq!(
            results.len(),
            count,
            "Should process all high-freq packets without dropping"
        );
    }
}
