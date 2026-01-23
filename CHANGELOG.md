# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **Modular Architecture**: Split monolithic `connection.rs` (3,700+ lines) into maintainable storage modules:
  - `types.rs`: Core data structures and Active FSM.
  - `prober.rs`: Intelligent auto-detection and smart framing logic.
  - `driver.rs`: Low-level I/O and loop management.
  - `reconnect.rs`: Robust auto-reconnection with exponential backoff.
  - `connection.rs`: Facade for orchestration.
- **Active FSM Driver**: Replaced "Passive Observer" FSM with "Active Driver" FSM.
  - **Thread Safety**: Unified `state` and `lock` into a single `AtomicU8` (`AtomicConnectionState`).
  - **Correctness**: Transitions acts as locks (Compare-and-Swap), eliminating race conditions between flags and state.
  - **Maintainability**: Centralized state transition logic; impossible to be in an invalid state.
- **Helper methods**: added `to_u8`/`from_u8` serialization for atomic state storage.

### Fixed
- **State Transition Bug**: Allowed `AutoReconnecting -> Disconnecting` transition.
  - User can now cancel an auto-reconnect loop by clicking Disconnect.
- **Compilation**: Fixed missing constants and async logic in probe recovery.
- **Test Coverage**: Restored comprehensive integration test suite for connection flows.

### Changed
- Refactored `ConnectionManager` to use `AtomicConnectionState` instead of scattered `AtomicBool` flags (`is_connecting`, `is_disconnecting`).
- Widened exports in `connection.rs` to ensure all auxiliary types are available to the application.

---

## [0.1.0] - 2025-01-20

### Added
- **MAVLink Support**: Full MAVLink v1/v2 decoding with auto-detection
- **Auto-Baud**: Smart probing (passive first) for 1.5M/1M baud support
- **Dashboard**: Specialized MAVLink telemetry view with Heartbeat/System tracking
- **Safety**: Buffer overflow protection and DoS mitigation (50 system cap)
- **Performance**: Optimized rendering and event processing loops
- **UI**: "FutureTerm" branding, Status Lights, and Dark Mode

### Fixed
- **Reconnection**: Robust hot-unplug/replug detection
- **Memory**: Capped limits for event buffers
- **Parsing**: Strict CRC and Magic Byte validation

---

## Development Guidelines

See `gemini.md` for complete development workflow documentation.

**Key Principle**: All code must pass strict quality checks before merging:
- ✅ No unwrap/expect/panic
- ✅ No unsafe indexing
- ✅ Formatted with rustfmt
- ✅ Clean clippy with denied lints
