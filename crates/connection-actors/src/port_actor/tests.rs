use super::*;
use futures::stream::StreamExt;

fn create_test_actor() -> (
    PortActor,
    mpsc::Receiver<StateMessage>,
    mpsc::Receiver<SystemEvent>,
) {
    let (state_tx, state_rx) = mpsc::channel(100);
    let (event_tx, event_rx) = mpsc::channel(100);

    let actor = PortActor::new(state_tx, event_tx);
    (actor, state_rx, event_rx)
}

#[tokio::test]
async fn test_initial_state() {
    let (actor, _, _) = create_test_actor();
    assert!(actor.active_port.is_none());
}

#[tokio::test]
async fn test_open_port_success() {
    let (mut actor, mut state_rx, mut event_rx) = create_test_actor();

    let port = actor_protocol::SerialPortInfo::new("/dev/ttyUSB0".into(), None, None);
    actor
        .handle_open(port.clone(), 115200, "8N1".into(), false)
        .await
        .unwrap();

    // Port should be marked as open
    assert_eq!(actor.active_port, Some("/dev/ttyUSB0".to_string()));

    // Should notify StateActor
    let state_msg = state_rx.next().await.unwrap();
    match state_msg {
        StateMessage::ConnectionEstablished => {}
        _ => panic!("Wrong message"),
    }

    // Should emit status event
    let event = event_rx.next().await.unwrap();
    match event {
        SystemEvent::StatusUpdate { message } => {
            assert!(message.contains("Connected"));
            assert!(message.contains("115200"));
        }
        _ => panic!("Wrong event"),
    }
}

#[tokio::test]
async fn test_close_port() {
    let (mut actor, _, mut event_rx) = create_test_actor();

    // Simulate open port
    actor.active_port = Some("/dev/ttyUSB0".to_string());

    actor.handle_close().await.unwrap();

    // Port should be closed
    assert!(actor.active_port.is_none());

    // Should emit close event
    let event = event_rx.next().await.unwrap();
    match event {
        SystemEvent::StatusUpdate { message } => {
            assert_eq!(message, "Port closed");
        }
        _ => panic!("Wrong event"),
    }
}

#[tokio::test]
async fn test_write_when_open() {
    let (mut actor, _, mut event_rx) = create_test_actor();

    actor.active_port = Some("/dev/ttyUSB0".to_string());

    actor.handle_write(vec![1, 2, 3]).await.unwrap();

    // Should emit TX activity
    let event = event_rx.next().await.unwrap();
    match event {
        SystemEvent::TxActivity => {}
        _ => panic!("Wrong event"),
    }
}

#[tokio::test]
async fn test_write_when_closed_returns_ok() {
    // New error handling: Expected State pattern
    let (mut actor, _, _) = create_test_actor();

    // Write when closed should return Ok (not an error)
    let result = actor.handle_write(vec![1, 2, 3]).await;
    assert!(result.is_ok());
}

#[tokio::test]
async fn test_cannot_open_twice() {
    let (mut actor, _, _) = create_test_actor();

    actor.active_port = Some("/dev/ttyUSB0".to_string());

    let port = actor_protocol::SerialPortInfo::new("/dev/ttyUSB1".into(), None, None);
    let result = actor.handle_open(port, 115200, "8N1".into(), false).await;

    assert!(result.is_err());
}

#[tokio::test]
async fn test_shutdown_closes_port() {
    let (mut actor, _, mut event_rx) = create_test_actor();

    actor.active_port = Some("/dev/ttyUSB0".to_string());

    actor.shutdown().await;

    assert!(actor.active_port.is_none());

    // Should emit close event
    let event = event_rx.next().await.unwrap();
    match event {
        SystemEvent::StatusUpdate { message } => {
            assert_eq!(message, "Port closed");
        }
        _ => panic!("Wrong event"),
    }
}

#[tokio::test]
async fn test_close_sends_connection_closed_message() {
    let (mut actor, mut state_rx, _) = create_test_actor();

    actor.active_port = Some("/dev/ttyUSB0".to_string());

    actor.handle_close().await.unwrap();

    // Should send ConnectionClosed to StateActor
    let state_msg = state_rx.next().await.unwrap();
    match state_msg {
        StateMessage::ConnectionClosed => {}
        _ => panic!("Expected ConnectionClosed, got {:?}", state_msg),
    }
}

#[tokio::test]
async fn test_close_when_already_closed_is_idempotent() {
    let (mut actor, _, _) = create_test_actor();

    // Close when already closed should succeed
    let result1 = actor.handle_close().await;
    assert!(result1.is_ok());

    let result2 = actor.handle_close().await;
    assert!(result2.is_ok());
}

#[tokio::test]
async fn test_parse_framing_8n1() {
    let config = PortActor::parse_framing("8N1", 115200).unwrap();
    assert_eq!(config.baud_rate, 115200);
    assert_eq!(config.data_bits, 8);
    assert_eq!(config.parity, core_types::Parity::None);
    assert_eq!(config.stop_bits, core_types::StopBits::One);
}

#[tokio::test]
async fn test_parse_framing_7e1() {
    let config = PortActor::parse_framing("7E1", 9600).unwrap();
    assert_eq!(config.baud_rate, 9600);
    assert_eq!(config.data_bits, 7);
    assert_eq!(config.parity, core_types::Parity::Even);
    assert_eq!(config.stop_bits, core_types::StopBits::One);
}

#[tokio::test]
async fn test_parse_framing_8e1() {
    let config = PortActor::parse_framing("8E1", 57600).unwrap();
    assert_eq!(config.baud_rate, 57600);
    assert_eq!(config.data_bits, 8);
    assert_eq!(config.parity, core_types::Parity::Even);
    assert_eq!(config.stop_bits, core_types::StopBits::One);
}

#[tokio::test]
async fn test_parse_framing_invalid_format() {
    let result = PortActor::parse_framing("INVALID", 19200);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("must be 3 characters"));
}

#[tokio::test]
async fn test_parse_framing_invalid_data_bits() {
    let result = PortActor::parse_framing("5N1", 115200);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("must be 7 or 8"));
}

#[tokio::test]
async fn test_parse_framing_invalid_parity() {
    let result = PortActor::parse_framing("8X1", 115200);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("must be N, E, or O"));
}

#[tokio::test]
async fn test_parse_framing_invalid_stop_bits() {
    let result = PortActor::parse_framing("8N3", 115200);
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(err.contains("must be 1 or 2"));
}

#[tokio::test]
async fn test_parse_framing_case_insensitive() {
    // Lowercase should work
    let config_lower = PortActor::parse_framing("8n1", 115200).unwrap();
    assert_eq!(config_lower.parity, core_types::Parity::None);

    // Mixed case
    let config_mixed = PortActor::parse_framing("7e1", 9600).unwrap();
    assert_eq!(config_mixed.parity, core_types::Parity::Even);

    let config_odd = PortActor::parse_framing("8o2", 57600).unwrap();
    assert_eq!(config_odd.parity, core_types::Parity::Odd);
}

#[test]
fn test_backoff_calculation_increases() {
    // Test that backoff delay increases with attempts
    let delay1 = crate::backoff::calculate_retry_delay(1);
    let delay2 = crate::backoff::calculate_retry_delay(2);
    let delay3 = crate::backoff::calculate_retry_delay(3);

    assert!(delay2 > delay1);
    assert!(delay3 > delay2);
}

#[test]
fn test_backoff_calculation_caps_at_max() {
    // Test that backoff delay is capped
    let delay_high = crate::backoff::calculate_retry_delay(50);
    let delay_higher = crate::backoff::calculate_retry_delay(100);

    // Should be capped, so they should be equal
    assert_eq!(delay_high, delay_higher);
}
