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
    mavlink::write_v1_msg(&mut writer, header, &msg).expect("Write 1 failed");
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
