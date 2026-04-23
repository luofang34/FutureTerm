#![allow(clippy::panic, clippy::unwrap_used, clippy::indexing_slicing)]

use super::*;
use futures::stream::StreamExt;

#[tokio::test]
async fn test_channel_manager_creation() {
    let (_manager, _handles) = ChannelManager::new();
    // Just verify it can be created
}

#[tokio::test]
async fn test_send_connect_command() {
    let (manager, mut handles) = ChannelManager::new();

    let cmd = UiCommand::Connect {
        port: actor_protocol::SerialPortInfo::new("/dev/ttyUSB0".into(), None, None),
        baud: 115200,
        framing: "8N1".to_string(),
    };

    manager.send_command(cmd).unwrap();

    // Verify message was routed to StateActor
    let msg = handles.state_rx.next().await.unwrap();
    match msg {
        StateMessage::UiCommand(UiCommand::Connect { baud, framing, .. }) => {
            assert_eq!(baud, 115200);
            assert_eq!(framing, "8N1");
        }
        _ => panic!("Wrong message type"),
    }
}

#[tokio::test]
async fn test_send_write_command() {
    let (manager, mut handles) = ChannelManager::new();

    let cmd = UiCommand::WriteData {
        data: vec![1, 2, 3],
    };

    manager.send_command(cmd).unwrap();

    // Verify message was routed to PortActor
    let msg = handles.port_rx.next().await.unwrap();
    match msg {
        PortMessage::Write { data } => {
            assert_eq!(data, vec![1, 2, 3]);
        }
        _ => panic!("Wrong message type"),
    }
}

#[tokio::test]
async fn test_event_receiver() {
    let (mut manager, mut handles) = ChannelManager::new();

    // Simulate an actor sending an event
    handles
        .event_tx
        .try_send(SystemEvent::StatusUpdate {
            message: "Test".into(),
        })
        .ok();

    // Drop handles to close channels
    drop(handles);

    // Receive event
    let event = manager.event_receiver().next().await.unwrap();
    match event {
        SystemEvent::StatusUpdate { message } => {
            assert_eq!(message, "Test");
        }
        _ => panic!("Wrong event type"),
    }
}

#[tokio::test]
async fn test_actor_to_actor_messaging() {
    let (manager, mut handles) = ChannelManager::new();

    // Get a clone of the state sender (as another actor would)
    let mut state_tx = manager.state_sender();

    // Simulate ProbeActor sending ProbeComplete to StateActor
    state_tx
        .try_send(StateMessage::ProbeComplete {
            baud: 115200,
            framing: "8N1".into(),
            protocol: Some("mavlink".into()),
            initial_data: vec![0, 1, 2],
        })
        .ok();

    // Verify StateActor receives it
    let msg = handles.state_rx.next().await.unwrap();
    match msg {
        StateMessage::ProbeComplete {
            baud,
            framing,
            protocol,
            initial_data,
        } => {
            assert_eq!(baud, 115200);
            assert_eq!(framing, "8N1");
            assert_eq!(protocol, Some("mavlink".into()));
            assert_eq!(initial_data, vec![0, 1, 2]);
        }
        _ => panic!("Wrong message type"),
    }
}
