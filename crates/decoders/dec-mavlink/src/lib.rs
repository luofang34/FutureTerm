use core_types::{DecodedEvent, Decoder, Frame};
use mavlink::{MavHeader, Message};
// Use std::io traits


pub struct MavlinkDecoder {
    buffer: Vec<u8>,
}

impl MavlinkDecoder {
    pub fn new() -> Self {
        Self {
            buffer: Vec::with_capacity(1024),
        }
    }

    fn parse_message(&self, header: &MavHeader, body: &mavlink::common::MavMessage) -> DecodedEvent {
        let msg_name = body.message_name();
        let mut event = DecodedEvent::new(0, "MAVLink", msg_name); // Timestamp set later
        
        event.confidence = 1.0;
        
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
            },
            mavlink::common::MavMessage::ATTITUDE(msg) => {
                event = event.with_field("roll", msg.roll);
                event = event.with_field("pitch", msg.pitch);
                event = event.with_field("yaw", msg.yaw);
            },
            mavlink::common::MavMessage::GLOBAL_POSITION_INT(msg) => {
                event = event.with_field("lat", msg.lat as f64 / 1e7);
                event = event.with_field("lon", msg.lon as f64 / 1e7);
                event = event.with_field("alt", msg.alt as f64 / 1000.0);
                event = event.with_field("relative_alt", msg.relative_alt as f64 / 1000.0);
            },
            mavlink::common::MavMessage::GPS_RAW_INT(msg) => {
                 event = event.with_field("fix_type", format!("{:?}", msg.fix_type));
                 event = event.with_field("lat", msg.lat as f64 / 1e7);
                 event = event.with_field("lon", msg.lon as f64 / 1e7);
                 event = event.with_field("alt", msg.alt as f64 / 1000.0);
                 event = event.with_field("satellites_visible", msg.satellites_visible as i64);
            },
             mavlink::common::MavMessage::SYS_STATUS(msg) => {
                event = event.with_field("voltage_battery", msg.voltage_battery as f64 / 1000.0);
                event = event.with_field("current_battery", msg.current_battery as i64);
                event = event.with_field("battery_remaining", msg.battery_remaining as i64);
                event = event.with_field("load", msg.load as f64 / 10.0);
            },
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

        // Process buffer
        loop {
            // Minimal length check (header)
            if self.buffer.len() < 8 {
                break;
            }

            // Look for magic byte
            // v1 (0xFE) or v2 (0xFD)
            let magic_idx = self.buffer.iter().position(|&b| b == 0xFE || b == 0xFD);
            
            if let Some(idx) = magic_idx {
                // Discard garbage before magic byte
                if idx > 0 {
                    // leptos::logging::log!("MAVLink: Garbage skipped: {} bytes", idx);
                    self.buffer.drain(0..idx);
                }
            } else {
                if self.buffer.len() > 200 {
                     // leptos::logging::log!("MAVLink: Buffer full ({}) no magic found, clearing", self.buffer.len());
                     self.buffer.clear();
                }
                break;
            }
            
            let magic = self.buffer[0];
            let payload_len = self.buffer[1] as usize;
            
            let expected_len = if magic == 0xFE { // v1
                8 + payload_len
            } else { // v2 (0xFD)
                12 + payload_len // Assumes no signature strictly here, but let's be lenient
            };
            
            // Refine expected len for v2 signature
            let mut total_len = expected_len;
             if magic == 0xFD && self.buffer.len() >= 3 {
                 let incompat_flags = self.buffer[2];
                 if incompat_flags & 0x01 != 0 {
                     total_len += 13;
                 }
            }

            if self.buffer.len() < total_len {
                break; // Wait for more data
            }

            // Try parse using the wrapper
            // Try parse using slice reader (requires std::io::Read for &[u8])
            let mut reader = &self.buffer[0..total_len];
            
            let parse_result = if magic == 0xFE {
                mavlink::read_v1_msg::<mavlink::common::MavMessage, _>(&mut reader)
            } else {
                mavlink::read_v2_msg::<mavlink::common::MavMessage, _>(&mut reader)
            };

            match parse_result {
                Ok((header, body)) => {
                    let mut event = self.parse_message(&header, &body);
                    event.timestamp_us = frame.timestamp_us; // Inherit timestamp
                    let ver_str = if magic == 0xFE { "v1" } else { "v2" };
                    event = event.with_field("version", ver_str.to_string());
                    results.push(event);
                    
                    // Remove consumed bytes
                    self.buffer.drain(0..total_len);
                },
                Err(_) => {
                    // CRC fail or parse error?
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
        // Manual construction of a packet byte buffer to avoid dependency on crate writing (which might need std::io::Write)
        // Or we use the crate? 
        // With embedded feature, write_v1_msg takes embedded::Write.
        // We can implement a ByteVecWriter.
        
        // pre-canned heartbeat v1 for Ardupilot
        // FE 09 00 01 01 00 00 00 00 00 02 03 51 04 03 1C 7F (CRC is example)
        // Let's rely on garbage handling test mostly, or try to init from struct if possible without std write.
        
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
        
        assert_eq!(results.len(), 1, "Should have found 1 packet despite garbage");
        assert_eq!(results[0].summary, "HEARTBEAT");
        
        // Verify buffer is clean (trailing junk might remain or be consumed if not enough for header)
        // logic: invalid magic -> drain 1. 0xFF is invalid magic start (if not FE/FD).
        // 0xFF != FE/FD. Drains. Buffer empty.
    }
}
