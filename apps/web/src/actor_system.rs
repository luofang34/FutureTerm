use actor_runtime::{spawn_actor, ChannelManager};
use connection_actors::{ProbeActor, ReconnectActor, StateActor};

#[cfg(target_arch = "wasm32")]
use connection_actors::PortActor;

/// Initialize the complete actor system
///
/// This creates all 4 actors and wires them together via channels.
/// Returns a ChannelManager that can be used to create an ActorBridge.
pub fn create_actor_system() -> ChannelManager {
    let (manager, handles) = ChannelManager::new();

    // Create actors
    let state_actor = StateActor::new(
        manager.port_sender(),
        manager.probe_sender(),
        manager.reconnect_sender(),
        handles.event_tx.clone(),
        manager.state_sender(),
    );

    let probe_actor = ProbeActor::new(manager.state_sender(), handles.event_tx.clone());

    let reconnect_actor = ReconnectActor::new(manager.state_sender(), handles.event_tx.clone());

    // Spawn all actors
    spawn_actor(state_actor, handles.state_rx, handles.event_tx.clone());

    #[cfg(target_arch = "wasm32")]
    {
        let port_actor = PortActor::new(manager.state_sender(), handles.event_tx.clone());
        spawn_actor(port_actor, handles.port_rx, handles.event_tx.clone());
    }

    spawn_actor(probe_actor, handles.probe_rx, handles.event_tx.clone());
    spawn_actor(reconnect_actor, handles.reconnect_rx, handles.event_tx);

    manager
}
