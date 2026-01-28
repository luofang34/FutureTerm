//! # Connection Actors
//!
//! Core actors for managing serial port connections.
//!
//! ## Actors
//!
//! - **StateActor**: Manages FSM state transitions and coordinates other actors
//! - **PortActor**: Handles I/O operations and port lifecycle
//! - **ProbeActor**: Performs auto-detection of baud rate and protocols
//! - **ReconnectActor**: Manages USB device hotplug and auto-reconnection

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::todo
)]

pub mod backoff;
pub mod constants;
pub mod data_processing;
#[cfg(target_arch = "wasm32")]
pub mod port_actor;
pub mod probe_actor;
pub mod reconnect_actor;
pub mod state_actor;

#[cfg(target_arch = "wasm32")]
pub use port_actor::PortActor;
pub use probe_actor::ProbeActor;
pub use reconnect_actor::{DeviceIdentity, ReconnectActor, ReconnectConfig};
pub use state_actor::StateActor;
