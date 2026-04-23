use super::create_test_actor;
use actor_protocol::{ConnectionState, SystemEvent};
use actor_runtime::{Actor, PortMessage, StateMessage};
use futures::stream::StreamExt;

#[tokio::test]
async fn test_device_reappeared_triggers_reconnection() {
    let (mut actor, mut port_rx, _, _, mut event_rx) = create_test_actor();

    // Simulate device lost
    actor.state = ConnectionState::DeviceLost;
    actor.pending_baud = 115200;

    // Simulate device reappearing
    let port =
        actor_protocol::SerialPortInfo::new("/dev/ttyUSB0".into(), Some(0x1234), Some(0x5678));
    actor
        .handle(StateMessage::DeviceReappeared { port: port.clone() })
        .await
        .unwrap();

    // Should transition to AutoReconnecting
    let event = event_rx.next().await.unwrap();
    match event {
        SystemEvent::StateChanged { state } => {
            assert_eq!(state, ConnectionState::AutoReconnecting);
        }
        _ => panic!("Wrong event"),
    }

    // Should send Open to PortActor
    let port_msg = port_rx.next().await.unwrap();
    match port_msg {
        PortMessage::Open { port: p, baud, .. } => {
            assert_eq!(p.path, "/dev/ttyUSB0");
            assert_eq!(baud, 115200);
        }
        _ => panic!("Wrong message"),
    }

    // State should still be AutoReconnecting (waits for ConnectionEstablished)
    assert_eq!(actor.state, ConnectionState::AutoReconnecting);
}

#[tokio::test]
async fn test_device_reappeared_ignored_when_not_device_lost() {
    let (mut actor, mut port_rx, _, _, _) = create_test_actor();

    // State is Disconnected (not DeviceLost)
    actor.state = ConnectionState::Disconnected;

    let port = actor_protocol::SerialPortInfo::new("/dev/ttyUSB0".into(), None, None);
    actor
        .handle(StateMessage::DeviceReappeared { port })
        .await
        .unwrap();

    // Should NOT send any messages to PortActor
    assert!(port_rx.try_next().is_err());

    // State should remain Disconnected
    assert_eq!(actor.state, ConnectionState::Disconnected);
}

#[tokio::test]
async fn test_device_reappeared_uses_default_baud_when_not_set() {
    let (mut actor, mut port_rx, _, _, _) = create_test_actor();

    // Simulate device lost with no pending_baud set
    actor.state = ConnectionState::DeviceLost;
    actor.pending_baud = 0; // Not set

    let port =
        actor_protocol::SerialPortInfo::new("/dev/ttyUSB0".into(), Some(0x1234), Some(0x5678));
    actor
        .handle(StateMessage::DeviceReappeared { port })
        .await
        .unwrap();

    // Should use default 115200 baud
    let port_msg = port_rx.next().await.unwrap();
    match port_msg {
        PortMessage::Open { baud, .. } => {
            assert_eq!(baud, 115200); // Default baud rate
        }
        _ => panic!("Wrong message"),
    }
}

#[tokio::test]
async fn test_connection_lost_closes_port() {
    let (mut actor, mut port_rx, _, _, mut event_rx) = create_test_actor();

    // Simulate connected state
    actor.state = ConnectionState::Connected;

    // Handle connection lost (USB disconnect)
    actor.handle(StateMessage::ConnectionLost).await.unwrap();

    // Should send Close to PortActor to clean up
    let port_msg = port_rx.next().await.unwrap();
    match port_msg {
        PortMessage::Close => {}
        _ => panic!("Expected Close message, got {:?}", port_msg),
    }

    // Should transition to DeviceLost
    let event = event_rx.next().await.unwrap();
    match event {
        SystemEvent::StateChanged { state } => {
            assert_eq!(state, ConnectionState::DeviceLost);
        }
        _ => panic!("Wrong event"),
    }

    assert_eq!(actor.state, ConnectionState::DeviceLost);
}

#[tokio::test]
async fn test_connection_established_from_auto_reconnecting() {
    let (mut actor, _, _, _, mut event_rx) = create_test_actor();

    // Simulate auto-reconnecting state (after USB replug)
    actor.state = ConnectionState::AutoReconnecting;
    actor.pending_port = Some(actor_protocol::SerialPortInfo::new(
        "/dev/ttyUSB0".into(),
        Some(0x1234),
        Some(0x5678),
    ));
    actor.pending_baud = 115200;
    actor.operation_sequence = 1; // Simulate operation ID

    // Port opens successfully and sends ConnectionEstablished
    actor.handle_connection_established(1).await.unwrap();

    // Should transition to Connected
    assert_eq!(actor.state, ConnectionState::Connected);

    // Should emit state change event
    let event = event_rx.next().await.unwrap();
    match event {
        SystemEvent::StateChanged { state } => {
            assert_eq!(state, ConnectionState::Connected);
        }
        _ => panic!("Wrong event"),
    }
}

#[tokio::test]
async fn test_operation_timeout_connecting() {
    let (mut actor, mut port_rx, _, _, mut event_rx) = create_test_actor();

    // Transition to Connecting
    actor.state = ConnectionState::Connecting;

    // Simulate timeout message
    actor
        .handle(StateMessage::OperationTimeout {
            operation: "Connecting".to_string(),
            state: ConnectionState::Connecting,
        })
        .await
        .unwrap();

    // Should send error event
    let event = event_rx.next().await.unwrap();
    match event {
        SystemEvent::Error { message } => {
            assert!(message.contains("Connecting"));
            assert!(message.contains("timed out"));
        }
        _ => panic!("Expected Error event, got {:?}", event),
    }

    // Should send Close to PortActor
    let port_msg = port_rx.next().await.unwrap();
    match port_msg {
        PortMessage::Close => {}
        _ => panic!("Expected Close message"),
    }

    // Should transition to Disconnecting
    assert_eq!(actor.state, ConnectionState::Disconnecting);
}

#[tokio::test]
async fn test_operation_timeout_ignored_after_state_change() {
    let (mut actor, _, _, _, mut event_rx) = create_test_actor();

    // Transition to Connecting, then immediately to Connected
    actor.state = ConnectionState::Connected;

    // Simulate timeout message for old state
    actor
        .handle(StateMessage::OperationTimeout {
            operation: "Connecting".to_string(),
            state: ConnectionState::Connecting,
        })
        .await
        .unwrap();

    // Timeout should be ignored (no error event)
    // Consume state change events from previous transitions
    while let Ok(Some(event)) = event_rx.try_next() {
        if let SystemEvent::Error { .. } = event {
            panic!("Should not send error for stale timeout");
        }
        // Ignore other events
    }

    // Should remain in Connected state
    assert_eq!(actor.state, ConnectionState::Connected);
}

#[tokio::test]
async fn test_operation_timeout_disconnecting() {
    let (mut actor, _, _, _, mut event_rx) = create_test_actor();

    // Transition to Disconnecting
    actor.state = ConnectionState::Disconnecting;

    // Simulate timeout message
    actor
        .handle(StateMessage::OperationTimeout {
            operation: "Disconnecting".to_string(),
            state: ConnectionState::Disconnecting,
        })
        .await
        .unwrap();

    // Should send error event
    let event = event_rx.next().await.unwrap();
    match event {
        SystemEvent::Error { message } => {
            assert!(message.contains("Disconnecting"));
        }
        _ => panic!("Expected Error event"),
    }

    // Should force transition to Disconnected
    assert_eq!(actor.state, ConnectionState::Disconnected);
}
