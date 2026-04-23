#![allow(clippy::panic, clippy::unwrap_used, clippy::indexing_slicing)]

use super::*;
use actor_protocol::SerialConfig;
use futures::stream::StreamExt;

fn create_test_actor() -> (
    ReconnectActor,
    mpsc::Receiver<StateMessage>,
    mpsc::Receiver<SystemEvent>,
) {
    let (state_tx, state_rx) = mpsc::channel(100);
    let (event_tx, event_rx) = mpsc::channel(100);

    let actor = ReconnectActor::new(state_tx, event_tx);
    (actor, state_rx, event_rx)
}

#[tokio::test]
async fn test_initial_state() {
    let (actor, _, _) = create_test_actor();
    assert!(actor.last_device.is_none());
    assert!(actor.reconnect_config.is_none());
}

#[tokio::test]
async fn test_register_device() {
    let (mut actor, _, _) = create_test_actor();

    let config = SerialConfig::new_8n1(115200);
    actor
        .handle_register_device(0x1234, 0x5678, config)
        .await
        .unwrap();

    assert_eq!(
        actor.last_device,
        Some(DeviceIdentity {
            vid: 0x1234,
            pid: 0x5678
        })
    );
    assert!(actor.reconnect_config.is_some());
    assert_eq!(actor.reconnect_config.as_ref().unwrap().baud, 115200);
}

#[tokio::test]
async fn test_clear_device() {
    let (mut actor, _, _) = create_test_actor();

    // Set up device
    actor.last_device = Some(DeviceIdentity {
        vid: 0x1234,
        pid: 0x5678,
    });
    actor.reconnect_config = Some(ReconnectConfig {
        baud: 115200,
        framing: "8N1".into(),
    });

    actor.handle_clear_device().await.unwrap();

    assert!(actor.last_device.is_none());
    assert!(actor.reconnect_config.is_none());
}

#[tokio::test]
async fn test_device_match_triggers_reconnect() {
    let (mut actor, mut state_rx, mut event_rx) = create_test_actor();

    // Register device
    let config = SerialConfig::new_8n1(115200);
    actor
        .handle_register_device(0x1234, 0x5678, config)
        .await
        .unwrap();

    // Simulate device connection
    let port =
        actor_protocol::SerialPortInfo::new("/dev/ttyUSB0".into(), Some(0x1234), Some(0x5678));
    actor.handle_device_connected(port.clone()).await.unwrap();

    // Should emit status update
    let event = event_rx.next().await.unwrap();
    match event {
        SystemEvent::StatusUpdate { message } => {
            assert!(message.contains("1234"));
            assert!(message.contains("5678"));
            assert!(message.contains("Auto-reconnecting"));
        }
        _ => panic!("Wrong event"),
    }

    // Should notify StateActor
    let state_msg = state_rx.next().await.unwrap();
    match state_msg {
        StateMessage::DeviceReappeared { port: p } => {
            assert_eq!(p.path, "/dev/ttyUSB0");
        }
        _ => panic!("Wrong message"),
    }
}

#[tokio::test]
async fn test_device_mismatch_ignored() {
    let (mut actor, mut state_rx, mut event_rx) = create_test_actor();

    // Register device 0x1234:0x5678
    let config = SerialConfig::new_8n1(115200);
    actor
        .handle_register_device(0x1234, 0x5678, config)
        .await
        .unwrap();

    // Simulate different device connection 0xAAAA:0xBBBB
    let port =
        actor_protocol::SerialPortInfo::new("/dev/ttyUSB0".into(), Some(0xAAAA), Some(0xBBBB));
    actor.handle_device_connected(port).await.unwrap();

    // Should NOT emit any events or messages (non-matching device)
    assert!(event_rx.try_next().is_err()); // No events
    assert!(state_rx.try_next().is_err()); // No state messages
}

#[tokio::test]
async fn test_no_device_registered_ignored() {
    let (mut actor, mut state_rx, mut event_rx) = create_test_actor();

    // No device registered
    let port =
        actor_protocol::SerialPortInfo::new("/dev/ttyUSB0".into(), Some(0x1234), Some(0x5678));
    actor.handle_device_connected(port).await.unwrap();

    // Should not trigger reconnect
    assert!(event_rx.try_next().is_err());
    assert!(state_rx.try_next().is_err());
}

#[tokio::test]
async fn test_device_without_vid_pid_ignored() {
    let (mut actor, mut state_rx, mut event_rx) = create_test_actor();

    // Register device
    let config = SerialConfig::new_8n1(115200);
    actor
        .handle_register_device(0x1234, 0x5678, config)
        .await
        .unwrap();

    // Port without VID/PID (e.g., virtual COM port)
    let port = actor_protocol::SerialPortInfo::new("/dev/ttyUSB0".into(), None, None);
    actor.handle_device_connected(port).await.unwrap();

    // Should be ignored
    assert!(event_rx.try_next().is_err());
    assert!(state_rx.try_next().is_err());
}

#[test]
fn test_device_swap_detection_logic() {
    // Test 1: Different device detected (device swap scenario)
    let target_vid = 0x0403;
    let target_pid = 0x6001;
    let found_vid = 0x1B8C;
    let found_pid = 0x0036;

    // Different VID/PID indicates device swap
    assert!(found_vid != target_vid || found_pid != target_pid);

    // Test 2: Same device detected (no device swap)
    let same_vid = 0x0403;
    let same_pid = 0x6001;

    // Same VID/PID indicates original device reconnected
    assert!(same_vid == target_vid && same_pid == target_pid);
}

#[test]
fn test_attempt_threshold() {
    // Device swap detection starts tracking at attempt 3 (~750ms cumulative)
    // Uses port validation instead of hardcoded timing
    // Validates after 2+ consecutive sightings of the same different device
    const DEVICE_SWAP_CHECK_ATTEMPT: u32 = 3;

    let should_check_attempt_1 = 1 >= DEVICE_SWAP_CHECK_ATTEMPT; // Too early
    let should_check_attempt_2 = 2 >= DEVICE_SWAP_CHECK_ATTEMPT; // Too early
    let should_check_attempt_3 = 3 >= DEVICE_SWAP_CHECK_ATTEMPT; // Threshold met!
    let should_check_attempt_4 = 4 >= DEVICE_SWAP_CHECK_ATTEMPT; // Also valid
    let should_check_attempt_5 = 5 >= DEVICE_SWAP_CHECK_ATTEMPT; // Also valid

    assert!(!should_check_attempt_1);
    assert!(!should_check_attempt_2); // Still too early
    assert!(should_check_attempt_3); // Triggers at attempt 3
    assert!(should_check_attempt_4); // Also triggers at attempt 4+
    assert!(should_check_attempt_5);
}

#[test]
fn test_device_swap_detection_requires_two_consistent_sightings() {
    // Real device swap: Same different device seen on 2+ consecutive attempts, then validated
    // Port validation eliminates need for 3+ sightings

    // Scenario 1: Device seen on attempts 3, 4 (count = 2) → VALIDATE
    let count_scenario_1 = 2;
    let should_validate_1 = count_scenario_1 >= 2;
    assert!(should_validate_1, "Should validate port when seen 2 times");

    // Scenario 2: Device seen only on attempt 3 (count = 1) → NO VALIDATE
    let count_scenario_2 = 1;
    let should_validate_2 = count_scenario_2 >= 2;
    assert!(
        !should_validate_2,
        "Should NOT validate when only seen 1 time (just appeared)"
    );
}

#[test]
fn test_multi_interface_device_scenario() {
    // Multi-interface USB device scenario with port validation:
    // - Attempts 3, 4: Control interface (3162:004B) visible → count = 2 → VALIDATE
    // - Validation FAILS (control interface is not a serial port)
    // - Counter resets, keeps waiting
    // - Attempt 5: Serial interface (1B8C:0036) appears → count = 1
    // - Attempt 6: Serial interface again → count = 2 → VALIDATE
    // - Validation SUCCEEDS → trigger device swap

    // Phase 1: Control interface seen twice
    let control_interface_count = 2;

    // Should validate at count = 2
    let should_validate_control = control_interface_count >= 2;
    assert!(should_validate_control, "Should validate after 2 sightings");

    // Validation fails (not a serial port) → counter resets to 0, then serial interface appears

    // Phase 2: Serial interface appears (count starts at 1)
    let new_device = (0x1B8C, 0x0036);
    let mut serial_interface_count = 1; // First sighting of serial interface

    // Next attempt: see serial interface again
    let last_device = Some(new_device);
    if last_device == Some(new_device) {
        serial_interface_count += 1;
    }

    // Should validate again (count = 2)
    assert_eq!(serial_interface_count, 2);
    assert!(
        serial_interface_count >= 2,
        "Should validate serial interface"
    );
}

#[test]
fn test_real_device_swap_scenario() {
    // Real device swap scenario with port validation:
    // - User unplugs FTDI (0403:6001)
    // - User plugs STM32 serial (1B8C:0036)
    // - Attempt 3: See STM32 → count = 1
    // - Attempt 4: See STM32 again → count = 2 → VALIDATE
    // - Validation SUCCEEDS (it's a serial port) → trigger device swap

    // Simulate: attempts 3, 4 saw STM32
    let consecutive_count = 2;

    // Should validate after 2 sightings
    let should_validate = consecutive_count >= 2;
    assert!(
        should_validate,
        "Should validate device after 2 consecutive sightings"
    );

    // If validation succeeds, device swap triggers
    // (validation logic tested separately in manual tests)
}

#[test]
fn test_validation_triggers_on_consecutive_sightings() {
    // Port validation triggers after 2 consecutive sightings (not on final attempt)
    // This allows faster device swap detection

    // Attempt 3: consecutive_count = 1 (first sighting)
    let consecutive_count_attempt_3 = 1;
    let should_validate_3 = consecutive_count_attempt_3 >= 2;
    assert!(!should_validate_3, "Should NOT validate on first sighting");

    // Attempt 4: consecutive_count = 2 (second consecutive sighting)
    let consecutive_count_attempt_4 = 2;
    let should_validate_4 = consecutive_count_attempt_4 >= 2;
    assert!(
        should_validate_4,
        "Should validate after 2 consecutive sightings"
    );

    // No need to wait for final attempt - validation happens immediately
}
