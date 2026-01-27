//! # Actor Protocol
//!
//! Type-safe message definitions for the FutureTerm actor system.
//!
//! This crate defines all messages used for communication between actors
//! in the connection management system. It has zero dependencies on UI
//! frameworks (no Leptos) or WASM-specific APIs (no web_sys), making it
//! fully testable in native Rust environments.
//!
//! ## Architecture
//!
//! - **UiCommand**: Messages from UI → Actor System
//! - **SystemEvent**: Messages from Actor System → UI
//! - **InternalMessage**: Messages between actors (not serialized)
//! - **ConnectionState**: FSM state machine (pure logic, no side effects)
//!
//! ## Message Flow
//!
//! ```text
//! UI → UiCommand → StateActor → InternalMessage → Other Actors
//!                      ↓
//!                 SystemEvent → UI
//! ```

#![deny(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    clippy::todo
)]

pub mod errors;
pub mod messages;
pub mod state;

pub use errors::ActorError;
pub use messages::{
    FlowControl, InternalMessage, ParityMode, ProbeResult, SerialConfig, SerialPortInfo,
    SystemEvent, UiCommand,
};
pub use state::{ConnectionState, LOCK_FLAG, STATE_MASK};
