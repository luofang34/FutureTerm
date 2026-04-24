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
                    #[cfg(all(debug_assertions, target_arch = "wasm32"))]
                    web_sys::console::log_1(&format!("MAVLink parse failed: {:?}", _e).into());

                    // Skip to next magic byte candidate for O(n) resync
                    // (instead of advancing 1 byte which is O(n^2) on adversarial input)
                    let skip = self
                        .buffer
                        .get(1..)
                        .and_then(|rest| {
                            rest.iter()
                                .position(|&b| b == MAVLINK_V1_MAGIC || b == MAVLINK_V2_MAGIC)
                                .map(|pos| pos + 1)
                        })
                        .unwrap_or(self.buffer.len());
                    self.buffer.drain(0..skip);
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
mod tests;
