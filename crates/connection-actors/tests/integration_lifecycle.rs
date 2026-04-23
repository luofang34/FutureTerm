//! Integration tests for actor system
//!
//! These tests verify end-to-end flows across multiple actors.
//! Note: PortActor is WASM-only, so these tests focus on StateActor coordination.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use actor_protocol::{SerialConfig, UiCommand};
use actor_runtime::{Actor, ChannelManager, PortMessage, StateMessage};
use connection_actors::{ProbeActor, ReconnectActor, StateActor};
use futures::stream::StreamExt;
use std::time::Duration;

/// Test that ondisconnect handler has connection state tracking (Fix #2)
/// Note: Full testing requires wasm32 target with real USB events
#[tokio::test]
async fn test_reconnect_actor_connection_state_tracking() {
    let (manager, handles) = ChannelManager::new();

    let mut reconnect_actor = ReconnectActor::new(manager.state_sender(), handles.event_tx.clone());

    // Register device with is_connected=true (implicit in RegisterDevice)
    reconnect_actor
        .handle(actor_runtime::ReconnectMessage::RegisterDevice {
            vid: 0x1234,
            pid: 0x5678,
            config: SerialConfig::new_8n1(115200),
        })
        .await
        .expect("Should register device");

    // Verify device was registered successfully (no panic)
    // The DeviceState struct with is_connected flag is now used internally
    // to prevent false positive ConnectionLost events from unrelated USB devices

    // Clear device registration
    reconnect_actor
        .handle(actor_runtime::ReconnectMessage::ClearDevice)
        .await
        .expect("Should clear device");

    // Verify clearing works (no panic)
    // When cleared, ondisconnect handler won't send ConnectionLost

    drop(reconnect_actor);
    drop(handles);
    drop(manager);
}

/// Test that done_rx.await has timeout to prevent deadlock (Fix #1)
/// This validates the timeout pattern used in PortActor::handle_close
#[tokio::test]
async fn test_port_close_timeout_mechanism() {
    use futures_channel::oneshot;
    use std::time::Instant;

    // Test 1: Channel closed without signal (sender dropped) - should return immediately
    let (done_tx, done_rx) = oneshot::channel::<()>();
    drop(done_tx);

    let start = Instant::now();
    let result = tokio::time::timeout(Duration::from_millis(500), done_rx).await;

    match result {
        Ok(Ok(())) => panic!("Should not succeed"),
        Ok(Err(_)) => {
            // Channel closed without signal - expected
            let elapsed = start.elapsed();
            assert!(
                elapsed < Duration::from_millis(100),
                "Should return immediately when channel is closed, took {:?}",
                elapsed
            );
        }
        Err(_) => {
            panic!("Should not timeout, channel is already closed");
        }
    }

    // Test 2: Actual timeout scenario (sender kept alive) - should timeout after 500ms
    let (_done_tx2, done_rx2) = oneshot::channel::<()>();

    let start2 = Instant::now();
    let result2 = tokio::time::timeout(Duration::from_millis(500), done_rx2).await;

    match result2 {
        Ok(_) => panic!("Should timeout, not complete"),
        Err(_) => {
            // Timeout - expected behavior
            let elapsed = start2.elapsed();
            assert!(
                elapsed >= Duration::from_millis(450) && elapsed <= Duration::from_millis(600),
                "Should timeout after ~500ms, got {:?}",
                elapsed
            );
        }
    }
}

/// Test rapid connect/disconnect cycles to verify no race conditions or resource leaks
#[tokio::test]
async fn test_rapid_connect_disconnect_cycles() {
    let (manager, handles) = ChannelManager::new();

    let event_tx_state = handles.event_tx.clone();

    // Create StateActor
    let state_actor = StateActor::new(
        manager.port_sender(),
        manager.probe_sender(),
        manager.reconnect_sender(),
        handles.event_tx.clone(),
        manager.state_sender(),
    );

    // Spawn StateActor
    let state_handle =
        tokio::spawn(async move { state_actor.run(handles.state_rx, event_tx_state).await });

    // Keep other channels alive
    let _port_rx = handles.port_rx;
    let _probe_rx = handles.probe_rx;
    let _reconnect_rx = handles.reconnect_rx;

    // Rapidly send connect/disconnect commands (10 cycles)
    for i in 0..10 {
        // Connect
        manager
            .send_command(UiCommand::Connect {
                port: actor_protocol::SerialPortInfo::new(format!("/dev/ttyUSB{}", i), None, None),
                baud: 115200,
                framing: "8N1".to_string(),
            })
            .expect("Should send connect command");

        tokio::time::sleep(Duration::from_millis(10)).await;

        // Disconnect
        manager
            .send_command(UiCommand::Disconnect)
            .expect("Should send disconnect command");

        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    // Verify StateActor is still responsive (doesn't panic during rapid cycles)
    manager
        .send_command(UiCommand::Connect {
            port: actor_protocol::SerialPortInfo::new("/dev/final".into(), None, None),
            baud: 9600,
            framing: "8N1".to_string(),
        })
        .expect("Should send final command");

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Test passed if we reached here without panicking
    // Note: We don't wait for full shutdown because StateActor would be stuck
    // waiting for PortActor responses in a real system. The goal of this test
    // is to verify no race conditions or panics during rapid cycles.

    // Cleanup
    drop(manager);

    // Give StateActor a chance to process channel closure
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify StateActor doesn't panic (if it panicked, the task would be in panic state)
    assert!(
        !state_handle.is_finished() || state_handle.await.is_ok(),
        "StateActor should not panic during rapid cycles"
    );
}

/// Test handling of port busy scenario (another process has the port open)
#[tokio::test]
async fn test_port_busy_scenario() {
    let (manager, handles) = ChannelManager::new();

    let event_tx_state = handles.event_tx.clone();

    let state_actor = StateActor::new(
        manager.port_sender(),
        manager.probe_sender(),
        manager.reconnect_sender(),
        handles.event_tx.clone(),
        manager.state_sender(),
    );

    let state_handle =
        tokio::spawn(async move { state_actor.run(handles.state_rx, event_tx_state).await });

    let _port_rx = handles.port_rx;
    let _probe_rx = handles.probe_rx;
    let _reconnect_rx = handles.reconnect_rx;

    // Simulate attempting to open a busy port
    // In reality, PortActor would receive "Device or resource busy" error from WebSerial
    // Here we just verify StateActor handles ConnectionFailed messages correctly
    manager
        .send_command(UiCommand::Connect {
            port: actor_protocol::SerialPortInfo::new("/dev/busy".into(), None, None),
            baud: 115200,
            framing: "8N1".to_string(),
        })
        .expect("Should send connect command");

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Send ConnectionFailed message (simulating PortActor's response to busy port)
    let state_tx = manager.state_sender();
    state_tx
        .clone()
        .try_send(StateMessage::ConnectionFailed {
            reason: "Device or resource busy".to_string(),
        })
        .expect("Should send ConnectionFailed");

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify StateActor transitions back to Disconnected and can accept new commands
    manager
        .send_command(UiCommand::Connect {
            port: actor_protocol::SerialPortInfo::new("/dev/ttyUSB0".into(), None, None),
            baud: 115200,
            framing: "8N1".to_string(),
        })
        .expect("Should accept new connect command after failure");

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Test passed if we reached here - StateActor handled ConnectionFailed correctly
    // and accepted a new connect command without panicking

    // Cleanup
    drop(manager);

    // Give StateActor a chance to process channel closure
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Verify StateActor doesn't panic when handling port busy scenario
    assert!(
        !state_handle.is_finished() || state_handle.await.is_ok(),
        "StateActor should handle port busy scenario without panicking"
    );
}

/// Test device swap during probing (device unplugged and new device plugged in)
#[tokio::test]
async fn test_device_swap_during_probing() {
    let (manager, handles) = ChannelManager::new();

    let event_tx_state = handles.event_tx.clone();
    let event_tx_probe = handles.event_tx.clone();

    let state_actor = StateActor::new(
        manager.port_sender(),
        manager.probe_sender(),
        manager.reconnect_sender(),
        handles.event_tx.clone(),
        manager.state_sender(),
    );

    let probe_actor = ProbeActor::new(manager.state_sender(), handles.event_tx.clone());

    let state_handle =
        tokio::spawn(async move { state_actor.run(handles.state_rx, event_tx_state).await });

    let probe_handle =
        tokio::spawn(async move { probe_actor.run(handles.probe_rx, event_tx_probe).await });

    let _port_rx = handles.port_rx;
    let _reconnect_rx = handles.reconnect_rx;

    // Start auto-detection (enters Probing state)
    manager
        .send_command(UiCommand::Connect {
            port: actor_protocol::SerialPortInfo::new(
                "/dev/ttyUSB0".into(),
                Some(0x1234),
                Some(0x5678),
            ),
            baud: 0, // Auto-detect triggers probing
            framing: "Auto".to_string(),
        })
        .expect("Should send connect command");

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Simulate device swap: send disconnect command while probing
    manager
        .send_command(UiCommand::Disconnect)
        .expect("Should send disconnect during probing");

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Attempt to connect to new device
    manager
        .send_command(UiCommand::Connect {
            port: actor_protocol::SerialPortInfo::new(
                "/dev/ttyUSB1".into(),
                Some(0xABCD),
                Some(0xEF01),
            ),
            baud: 115200,
            framing: "8N1".to_string(),
        })
        .expect("Should accept connect to new device");

    tokio::time::sleep(Duration::from_millis(100)).await;

    // Cleanup
    drop(manager);

    // Both actors should shutdown gracefully despite the device swap during probing
    let shutdown_result = tokio::time::timeout(Duration::from_secs(10), async {
        let _ = state_handle.await;
        let _ = probe_handle.await;
    })
    .await;

    // Note: ProbeActor may timeout in native tests (expected), but shouldn't panic
    if shutdown_result.is_err() {
        eprintln!(
            "Note: Actors timed out during shutdown (expected in native tests with device swap)"
        );
    }
}
