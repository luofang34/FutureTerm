use super::create_test_actor;
use actor_protocol::{ActorError, ConnectionState, SystemEvent};
use actor_runtime::{Actor, PortMessage, ProbeMessage, StateMessage};
use futures::stream::StreamExt;

#[tokio::test]
async fn test_initial_state() {
    let (actor, _, _, _, _) = create_test_actor();
    assert_eq!(actor.state, ConnectionState::Disconnected);
}

#[tokio::test]
async fn test_connect_with_baud() {
    let (mut actor, mut port_rx, _, _, mut event_rx) = create_test_actor();

    let port = actor_protocol::SerialPortInfo::new("/dev/ttyUSB0".into(), None, None);
    actor
        .handle_connect(port.clone(), 115200, "8N1".to_string())
        .await
        .unwrap();

    // Should transition to Connecting
    assert_eq!(actor.state, ConnectionState::Connecting);

    // Should send Open to PortActor
    let port_msg = port_rx.next().await.unwrap();
    match port_msg {
        PortMessage::Open { baud, .. } => assert_eq!(baud, 115200),
        _ => panic!("Wrong message"),
    }

    // Should emit state change event
    let event = event_rx.next().await.unwrap();
    match event {
        SystemEvent::StateChanged { state } => {
            assert_eq!(state, ConnectionState::Connecting);
        }
        _ => panic!("Wrong event"),
    }
}

#[tokio::test]
async fn test_connect_with_auto_detect() {
    let (mut actor, _, mut probe_rx, _, _) = create_test_actor();

    let port = actor_protocol::SerialPortInfo::new("/dev/ttyUSB0".into(), None, None);
    actor
        .handle_connect(port.clone(), 0, "Auto".to_string())
        .await
        .unwrap();

    // Should transition to Probing
    assert_eq!(actor.state, ConnectionState::Probing);

    // Should send Start to ProbeActor
    let probe_msg = probe_rx.next().await.unwrap();
    match probe_msg {
        ProbeMessage::Start { .. } => {}
        _ => panic!("Wrong message"),
    }
}

#[tokio::test]
async fn test_disconnect_from_connected() {
    let (mut actor, mut port_rx, _, _, _) = create_test_actor();

    // Manually set to connected
    actor.state = ConnectionState::Connected;

    actor.handle_disconnect().await.unwrap();

    // Should send Close to PortActor
    let port_msg = port_rx.next().await.unwrap();
    match port_msg {
        PortMessage::Close => {}
        _ => panic!("Wrong message"),
    }

    // Should be in Disconnecting state (event-driven coordination)
    assert_eq!(actor.state, ConnectionState::Disconnecting);

    // Simulate PortActor confirming closure
    actor.handle(StateMessage::ConnectionClosed).await.unwrap();

    // Now should be disconnected
    assert_eq!(actor.state, ConnectionState::Disconnected);
}

#[tokio::test]
async fn test_connection_established() {
    let (mut actor, _, _, _, mut event_rx) = create_test_actor();

    actor.state = ConnectionState::Connecting;
    actor.operation_sequence = 1; // Simulate operation ID
    actor.handle_connection_established(1).await.unwrap();

    assert_eq!(actor.state, ConnectionState::Connected);

    // Should emit state change
    let event = event_rx.next().await.unwrap();
    match event {
        SystemEvent::StateChanged { state } => {
            assert_eq!(state, ConnectionState::Connected);
        }
        _ => panic!("Wrong event"),
    }
}

#[tokio::test]
async fn test_connection_failed() {
    let (mut actor, _, _, _, mut event_rx) = create_test_actor();

    actor.state = ConnectionState::Connecting;
    actor
        .handle_connection_failed("Port busy".into())
        .await
        .unwrap();

    assert_eq!(actor.state, ConnectionState::Disconnected);

    // Should emit error event
    let event = event_rx.next().await.unwrap();
    match event {
        SystemEvent::Error { message } => {
            assert!(message.contains("Port busy"));
        }
        _ => panic!("Wrong event"),
    }
}

#[tokio::test]
async fn test_connection_established_rejects_stale_operation() {
    let (mut actor, mut port_rx, _, _, _) = create_test_actor();

    actor.state = ConnectionState::Connecting;
    actor.operation_sequence = 5; // Expect operation ID 5

    // Try to establish connection with stale operation ID
    let result = actor.handle_connection_established(3).await;

    // Should reject stale operation
    assert!(result.is_err());
    assert_eq!(actor.state, ConnectionState::Connecting); // State unchanged

    // Should send Close message to PortActor to close orphan port
    let msg = port_rx.next().await.unwrap();
    match msg {
        PortMessage::Close => {} // Expected
        _ => panic!("Expected Close message, got {:?}", msg),
    }
}

#[tokio::test]
async fn test_invalid_transition_rejected() {
    let (mut actor, _, _, _, _) = create_test_actor();

    actor.state = ConnectionState::Disconnected;

    // Cannot go directly to Connected
    let result = actor.transition(ConnectionState::Connected);
    assert!(result.is_err());
}

#[tokio::test]
async fn test_probe_complete() {
    let (mut actor, mut port_rx, _, _, mut event_rx) = create_test_actor();

    actor.state = ConnectionState::Probing;
    // Set pending port (as would happen during connect)
    actor.pending_port = Some(actor_protocol::SerialPortInfo::new(
        "/dev/ttyUSB0".into(),
        None,
        None,
    ));

    actor
        .handle_probe_complete(115200, "8N1".into(), Some("mavlink".into()), vec![1, 2, 3])
        .await
        .unwrap();

    assert_eq!(actor.state, ConnectionState::Connecting);

    // Should have sent Open to PortActor
    let port_msg = port_rx.next().await.unwrap();
    match port_msg {
        PortMessage::Open { baud, framing, .. } => {
            assert_eq!(baud, 115200);
            assert_eq!(framing, "8N1");
        }
        _ => panic!("Wrong message"),
    }

    // Should emit status update
    let event = event_rx.next().await.unwrap();
    match event {
        SystemEvent::StatusUpdate { message } => {
            assert!(message.contains("mavlink"));
            assert!(message.contains("115200"));
        }
        _ => panic!("Wrong event"),
    }
}

#[tokio::test]
async fn test_disconnect_aborts_probe() {
    let (mut actor, mut port_rx, mut probe_rx, _, _) = create_test_actor();

    actor.state = ConnectionState::Probing;
    actor.handle_disconnect().await.unwrap();

    // Should send Abort to ProbeActor
    let probe_msg = probe_rx.next().await.unwrap();
    match probe_msg {
        ProbeMessage::Abort => {}
        _ => panic!("Wrong message"),
    }

    // Should send Close to PortActor
    let port_msg = port_rx.next().await.unwrap();
    match port_msg {
        PortMessage::Close => {}
        _ => panic!("Expected Close message"),
    }

    // Should be in Disconnecting state (event-driven coordination)
    assert_eq!(actor.state, ConnectionState::Disconnecting);

    // Simulate PortActor confirming closure
    actor.handle(StateMessage::ConnectionClosed).await.unwrap();

    // Now should be disconnected
    assert_eq!(actor.state, ConnectionState::Disconnected);
}

#[tokio::test]
async fn test_unexpected_message_error() {
    let (mut actor, _, _, _, _) = create_test_actor();

    // Try to connect when already connecting
    actor.state = ConnectionState::Connecting;
    let port = actor_protocol::SerialPortInfo::new("/dev/ttyUSB0".into(), None, None);
    let result: Result<(), ActorError> =
        actor.handle_connect(port, 115200, "8N1".to_string()).await;

    assert!(result.is_err());
    match result.unwrap_err() {
        ActorError::UnexpectedMessage { state, message } => {
            assert!(state.contains("Connecting"));
            assert_eq!(message, "Connect");
        }
        _ => panic!("Wrong error type"),
    }
}

#[tokio::test]
async fn test_probe_complete_ignored_when_disconnected() {
    // Fix #9 verification
    let (mut actor, _, _, _, _) = create_test_actor();

    // State is Disconnected
    actor.state = ConnectionState::Disconnected;

    // Simulate delayed ProbeComplete arriving after disconnect
    let result: Result<(), ActorError> = actor
        .handle_probe_complete(115200, "8N1".into(), None, vec![])
        .await;

    // Should be Ok(()) (ignored), not Err
    assert!(result.is_ok());
    assert_eq!(actor.state, ConnectionState::Disconnected);
}
