//! # Actor Runtime
//!
//! Provides the runtime infrastructure for the FutureTerm actor system.
//!
//! This crate defines:
//! - **Actor trait**: Base trait for all actors with lifecycle methods
//! - **Channel management**: Type-safe message routing between actors
//! - **Spawn utilities**: Helper functions for launching actors
//!
//! ## Architecture
//!
//! The actor runtime follows these principles:
//! - **Zero shared state**: Each actor owns its data
//! - **Message passing**: Actors communicate via typed messages
//! - **Sequential processing**: Messages are handled one at a time
//! - **Failure isolation**: Actor errors don't crash the system
//!
//! ## Example
//!
//! ```ignore
//! use actor_runtime::{Actor, spawn_actor, ChannelManager};
//!
//! // Create channel infrastructure
//! let (manager, handles) = ChannelManager::new();
//!
//! // Create and spawn actors
//! let state_actor = StateActor::new(/* ... */);
//! spawn_actor(state_actor, handles.state_rx, handles.event_tx.clone());
//!
//! // Send commands from UI
//! manager.send_command(UiCommand::Connect { /* ... */ });
//! ```

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::todo
)]

pub mod actor;
pub mod cancellation;
pub mod channels;
pub mod logging;
pub mod supervision;

pub use actor::{spawn_actor, Actor};
pub use channels::{
    ActorHandles, ChannelManager, PortMessage, ProbeMessage, ReconnectMessage, StateMessage,
};
pub use supervision::{spawn_timeout, SupervisionConfig, TimeoutHandle};

#[cfg(target_arch = "wasm32")]
pub use cancellation::{
    create_cancel_future, create_cancel_future_with_interval, race_with_cancellation,
    DEFAULT_CANCEL_POLL_MS,
};

#[cfg(target_arch = "wasm32")]
pub use channels::PortHandle;
