#![allow(clippy::panic, clippy::unwrap_used, clippy::indexing_slicing)]

use super::StateActor;
use actor_protocol::SystemEvent;
use actor_runtime::{PortMessage, ProbeMessage, ReconnectMessage};
use futures_channel::mpsc;

fn create_test_actor() -> (
    StateActor,
    mpsc::Receiver<PortMessage>,
    mpsc::Receiver<ProbeMessage>,
    mpsc::Receiver<ReconnectMessage>,
    mpsc::Receiver<SystemEvent>,
) {
    let (port_tx, port_rx) = mpsc::channel(100);
    let (probe_tx, probe_rx) = mpsc::channel(100);
    let (reconnect_tx, reconnect_rx) = mpsc::channel(100);
    let (event_tx, event_rx) = mpsc::channel(100);
    let (state_tx, _state_rx) = mpsc::channel(100);

    let actor = StateActor::new(port_tx, probe_tx, reconnect_tx, event_tx, state_tx);
    (actor, port_rx, probe_rx, reconnect_rx, event_rx)
}

mod state_transitions;
mod supervision;
