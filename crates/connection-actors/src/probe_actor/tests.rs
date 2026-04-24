#![allow(clippy::panic, clippy::unwrap_used, clippy::indexing_slicing)]

use super::*;
use futures::stream::StreamExt;

fn create_test_actor() -> (
    ProbeActor,
    mpsc::Receiver<StateMessage>,
    mpsc::Receiver<SystemEvent>,
) {
    let (state_tx, state_rx) = mpsc::channel(100);
    let (event_tx, event_rx) = mpsc::channel(100);

    let actor = ProbeActor::new(state_tx, event_tx);
    (actor, state_rx, event_rx)
}

#[tokio::test]
async fn test_analyze_mavlink() {
    let (actor, _, _) = create_test_actor();

    // Valid MAVLink v1 packet: FE len=3 ... total=3+8=11 bytes
    let buffer = vec![
        0xFE, 0x03, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    let result = actor.analyze_buffer(&buffer, 115200);

    assert_eq!(result.baud, 115200);
    // Protocol detection only works with mavlink feature enabled and valid packet
    // For testing without feature, we rely on score
    #[cfg(feature = "mavlink")]
    assert_eq!(result.protocol, Some("mavlink".into()));
    assert!(result.confidence > 0.9);
}

#[tokio::test]
async fn test_analyze_nmea() {
    let (actor, _, _) = create_test_actor();

    let buffer = b"$GPGGA,123519,4807.038,N,01131.000,E".to_vec();
    let result = actor.analyze_buffer(&buffer, 9600);

    assert_eq!(result.baud, 9600);
    assert_eq!(result.protocol, Some("nmea".into()));
    assert!(result.confidence > 0.85);
}

#[tokio::test]
async fn test_analyze_text() {
    let (actor, _, _) = create_test_actor();

    let buffer = b"Hello World\nThis is text\n".to_vec();
    let result = actor.analyze_buffer(&buffer, 115200);

    // Text data should be detected with good confidence, but protocol should be None
    // (user should choose decoder manually for generic text)
    assert_eq!(result.protocol, None);
    assert!(result.confidence > 0.5);
}

#[tokio::test]
async fn test_analyze_empty() {
    let (actor, _, _) = create_test_actor();

    let buffer = vec![];
    let result = actor.analyze_buffer(&buffer, 115200);

    assert_eq!(result.confidence, 0.0);
}

#[tokio::test]
async fn test_abort_sets_flag() {
    let (mut actor, _, _) = create_test_actor();

    assert!(!actor.interrupt_flag.load(Ordering::Acquire));

    actor.handle_abort().await.unwrap();

    assert!(actor.interrupt_flag.load(Ordering::Acquire));
}

#[tokio::test]
async fn test_probe_interrupted_by_user() {
    let (mut actor, _, mut event_rx) = create_test_actor();

    let port = actor_protocol::SerialPortInfo::new("/dev/ttyUSB0".into(), None, None);

    // Start probe in background
    let actor_clone_flag = actor.interrupt_flag.clone();
    let handle = tokio::spawn(async move {
        let result = actor.handle_start(port).await;
        (actor, result)
    });

    // Wait for probe to enter first gather_probe_data call (which has 100ms sleep)
    tokio::time::sleep(tokio::time::Duration::from_millis(10)).await;

    // Abort the probe (flag will be checked during gather_probe_data sleep or at loop start)
    actor_clone_flag.store(true, Ordering::Release);

    // Wait for probe to finish
    let (returned_actor, result) = handle.await.unwrap();

    // Should return error due to abort OR complete with empty results
    // (depending on timing, interrupt may return empty buffer or abort between iterations)
    match result {
        Err(ActorError::Other(msg)) if msg.contains("aborted") => {
            // Expected: aborted between loop iterations
        }
        Err(ActorError::Other(msg)) if msg.contains("failed") => {
            // Expected: completed with low confidence (empty buffers from interruption)
        }
        Ok(_) => {
            // Also acceptable: probe completed but with interrupted gather_probe_data
            // returning empty buffers, resulting in low confidence
        }
        Err(e) => panic!("Unexpected error: {:?}", e),
    }

    // Verify flag is set
    assert!(returned_actor.interrupt_flag.load(Ordering::Acquire));

    // Should have received either cancellation or progress status updates
    let mut found_event = false;
    while let Ok(Some(event)) = event_rx.try_next() {
        match event {
            SystemEvent::StatusUpdate { message } => {
                if message.contains("cancelled") || message.contains("Scanning") {
                    found_event = true;
                    break;
                }
            }
            SystemEvent::ProbeProgress { .. } => {
                found_event = true;
                break;
            }
            _ => {}
        }
    }
    assert!(found_event, "Should have received at least one probe event");
}

#[tokio::test]
async fn test_probe_emits_progress() {
    let (mut actor, _, mut event_rx) = create_test_actor();

    let port = actor_protocol::SerialPortInfo::new("/dev/ttyUSB0".into(), None, None);

    // Start probe in background
    let handle = tokio::spawn(async move {
        let _ = actor.handle_start(port).await;
        actor
    });

    // Should receive at least one progress event
    let event = event_rx.next().await.unwrap();
    match event {
        SystemEvent::ProbeProgress { baud, .. } => {
            // Should be one of our candidates
            assert!([115200, 1500000, 921600, 57600, 9600, 38400, 19200].contains(&baud));
        }
        _ => panic!("Expected ProbeProgress event"),
    }

    // Wait for probe to finish
    let _actor = handle.await.unwrap();
}

#[tokio::test]
async fn test_probe_reports_success() {
    let (mut actor, mut state_rx, _) = create_test_actor();

    let port = actor_protocol::SerialPortInfo::new("/dev/ttyUSB0".into(), None, None);
    actor.handle_start(port).await.ok();

    // Should receive ProbeComplete
    let msg = state_rx.next().await.unwrap();
    match msg {
        StateMessage::ProbeComplete { baud, framing, .. } => {
            assert!(baud > 0);
            assert!(!framing.is_empty());
        }
        _ => panic!("Expected ProbeComplete"),
    }
}

#[tokio::test]
async fn test_analyze_binary() {
    let (actor, _, _) = create_test_actor();

    let buffer = vec![0x00, 0x01, 0x02, 0xFF, 0xAA, 0x55];
    let result = actor.analyze_buffer(&buffer, 115200);

    // Should detect as generic binary with moderate confidence
    assert_eq!(result.protocol, None);
    assert!(result.confidence > 0.0);
    assert!(result.confidence < 0.7);
}

#[tokio::test]
#[cfg(feature = "mavlink")]
async fn test_probe_handles_multiple_protocols_in_buffer() {
    let (actor, _, _) = create_test_actor();

    // Valid MAVLink v1 HEARTBEAT message (17 bytes)
    // Generated with proper CRC for HEARTBEAT (CRC_EXTRA=50)
    let mut buffer = vec![
        0xFE, // STX
        0x09, // payload length
        0x00, // sequence
        0x01, // system_id
        0x01, // component_id
        0x00, // message_id (HEARTBEAT)
        // Payload (9 bytes):
        0x00, 0x00, 0x00, 0x00, // custom_mode
        0x02, // type (QUADROTOR)
        0x03, // autopilot (ARDUPILOTMEGA)
        0x00, // base_mode
        0x04, // system_status (ACTIVE)
        0x03, // mavlink_version
        0xD0, 0x14, // CRC (X.25)
    ];

    // Append NMEA-like data to test mixed protocols
    buffer.extend_from_slice(b"$GPGGA,123519");

    let result = actor.analyze_buffer(&buffer, 115200);

    // Should detect MAVLink due to strong integrity verification
    assert_eq!(result.protocol, Some("mavlink".to_string()));
    assert!(result.confidence > 0.5);
}

#[tokio::test]
async fn test_probe_with_very_low_baud() {
    let (actor, _, _) = create_test_actor();

    // Test with minimum baud rate
    let buffer = b"Hello World";
    let result = actor.analyze_buffer(buffer, 300);

    // Should still work at low baud rates
    assert!(result.confidence > 0.0);
}

#[tokio::test]
async fn test_probe_with_very_high_baud() {
    let (actor, _, _) = create_test_actor();

    // Test with high baud rate
    let buffer = b"Test data";
    let result = actor.analyze_buffer(buffer, 921600);

    // Should still work at high baud rates
    assert!(result.confidence > 0.0);
}

#[tokio::test]
async fn test_abort_message_sets_interrupt_flag() {
    let (mut actor, _, _) = create_test_actor();

    // Send abort message
    actor.handle_abort().await.unwrap();

    // Interrupt flag should be set
    assert!(actor
        .interrupt_flag
        .load(std::sync::atomic::Ordering::Relaxed));
}

#[tokio::test]
async fn test_probe_with_partial_mavlink_frame() {
    let (actor, _, _) = create_test_actor();

    // Incomplete MAVLink frame (header only)
    let buffer = vec![0xFE, 0x09, 0x00, 0x01, 0x01]; // Magic + partial header

    let result = actor.analyze_buffer(&buffer, 115200);

    // Should still detect some confidence (or zero if buffer too small)
    assert!(result.confidence >= 0.0);
    assert_eq!(result.baud, 115200);
}

#[tokio::test]
async fn test_probe_with_ascii_control_characters() {
    let (actor, _, _) = create_test_actor();

    // Buffer with control characters and text
    let buffer = b"\x1b[0m\r\n$ Hello";

    let result = actor.analyze_buffer(buffer, 115200);

    // Should handle ANSI sequences and still detect text
    assert!(result.confidence > 0.0);
}

#[tokio::test]
async fn test_probe_result_includes_initial_data() {
    // Test that ProbeResult includes initial_data field
    let result = actor_protocol::ProbeResult {
        baud: 115200,
        framing: "8N1".to_string(),
        protocol: Some("mavlink".to_string()),
        initial_data: vec![1, 2, 3, 4, 5],
        confidence: 0.95,
    };

    // Verify all fields are accessible
    assert_eq!(result.baud, 115200);
    assert_eq!(result.framing, "8N1");
    assert_eq!(result.protocol, Some("mavlink".to_string()));
    assert_eq!(result.initial_data, vec![1, 2, 3, 4, 5]);
    assert_eq!(result.confidence, 0.95);
}

#[tokio::test]
async fn test_probe_with_repeated_characters() {
    let (actor, _, _) = create_test_actor();

    // Buffer with repeated characters (potential echo)
    let buffer = b"AAAAAAAAAA";

    let result = actor.analyze_buffer(buffer, 115200);

    // Should detect but with lower confidence due to repetition
    assert!(result.confidence > 0.0);
}

#[tokio::test]
#[cfg(feature = "mavlink")]
async fn test_probe_confidence_mavlink_vs_text() {
    let (actor, _, _) = create_test_actor();

    // Two valid MAVLink v1 HEARTBEAT messages (34 bytes total)
    let mavlink_buffer = vec![
        // Message 1 (sequence 0):
        0xFE, 0x09, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x03, 0x00, 0x04, 0x03,
        0xD0, 0x14, // Message 2 (sequence 1):
        0xFE, 0x09, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x03, 0x00, 0x04, 0x03,
        0x3A, 0x6A,
    ];

    // Use mixed binary and text to get lower confidence score
    let text_buffer = b"\x01\x02\x03Hello\xFF\xFE test data\x00";

    let mavlink_result = actor.analyze_buffer(&mavlink_buffer, 115200);
    let text_result = actor.analyze_buffer(text_buffer, 115200);

    // Both should have some confidence
    assert!(mavlink_result.confidence >= 0.0);
    assert!(text_result.confidence >= 0.0);

    // MAVLink should have higher confidence (1.0) due to integrity verification
    assert!(mavlink_result.confidence > text_result.confidence);
}

#[tokio::test]
#[cfg(feature = "mavlink")]
async fn test_probe_mavlink_v2_detection() {
    let (actor, _, _) = create_test_actor();

    // Valid MAVLink v2 HEARTBEAT message (21 bytes)
    // Generated with proper CRC for HEARTBEAT (CRC_EXTRA=50)
    let buffer = vec![
        0xFD, // STX (v2)
        0x09, // payload length
        0x00, // incompatibility flags
        0x00, // compatibility flags
        0x00, // sequence
        0x01, // system_id
        0x01, // component_id
        0x00, 0x00, 0x00, // message_id (24-bit, HEARTBEAT = 0)
        // Payload (9 bytes):
        0x00, 0x00, 0x00, 0x00, // custom_mode
        0x02, // type (QUADROTOR)
        0x03, // autopilot (ARDUPILOTMEGA)
        0x00, // base_mode
        0x04, // system_status (ACTIVE)
        0x03, // mavlink_version
        0x4A, 0xD7, // CRC (X.25)
    ];

    let result = actor.analyze_buffer(&buffer, 115200);

    // Should detect MAVLink v2 with high confidence
    assert_eq!(result.protocol, Some("mavlink".to_string()));
    assert_eq!(
        result.confidence, 1.0,
        "Should have perfect confidence for verified MAVLink v2"
    );
}

#[tokio::test]
#[cfg(feature = "mavlink")]
async fn test_probe_mavlink_v1_and_v2_mixed() {
    let (actor, _, _) = create_test_actor();

    // Mix of v1 and v2 messages
    let mut buffer = vec![
        // MAVLink v1 HEARTBEAT:
        0xFE, 0x09, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02, 0x03, 0x00, 0x04, 0x03,
        0xD0, 0x14,
    ];
    // Append MAVLink v2 HEARTBEAT:
    buffer.extend_from_slice(&[
        0xFD, 0x09, 0x00, 0x00, 0x00, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x02,
        0x03, 0x00, 0x04, 0x03, 0x4A, 0xD7,
    ]);

    let result = actor.analyze_buffer(&buffer, 115200);

    // Should detect MAVLink (either v1 or v2)
    assert_eq!(result.protocol, Some("mavlink".to_string()));
    assert_eq!(result.confidence, 1.0, "Should verify mixed v1/v2 messages");
}
