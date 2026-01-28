# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.1] - 2026-01-27

### Changed

**Complete architecture rewrite**: Migrated from monolithic ConnectionManager to message-passing actor system with 4 specialized actors (StateActor, PortActor, ProbeActor, ReconnectActor). Zero shared mutable state, type-safe errors, framework-agnostic business logic. Full backward compatibility maintained via ActorBridge.

### Added

- **Supervision Timeouts**: Auto-recovery from hung operations (Probing 30s, Connecting 10s, AutoReconnecting 10s, Disconnecting 5s, Reconfiguring 10s)
- **TimeoutHandle**: Automatic cancellation prevents spurious timeout messages after operations complete
- **Operation Sequence Tracking**: Validates port responses against expected sequence, prevents orphaned resources
- **Smart Port Selection**: Auto-select when only one USB device has permission
- **Interruptible Probing**: User can cancel auto-detection immediately via Disconnect button

### Fixed

- **Port Resource Leaks**: Read loop cleanup coordination via done channel, prevents "Already open" errors on reconnect
- **TOCTOU Races**: Interrupt checks after every `await` in probe actor
- **Timeout Cleanup**: Active timeouts automatically cancelled on state transitions
- **USB Auto-Reconnect**: 5-second global timeout prevents indefinite waiting, 5 retries with exponential backoff
- **Parity Settings**: Framing parameters (8N1/8E1/7E1) now correctly applied on connection
- **Auto-Detect Toggle**: Fixed switching back to "Auto" baud from manual rate
- **Port Picker**: Fixed localStorage key mismatch preventing auto-device-selection
- **Global State Races**: Removed `PENDING_PORT` thread-local that caused rapid connect/disconnect bugs
- **Retry Logic**: Distinguish retriable errors (NetworkError, busy) from fatal errors (permission denied)
- **Actor Channels**: User-friendly error messages on system failures ("Please refresh the page")

### Improved

- **Test Coverage**: 176 tests (69 actor, 18 runtime, 14 core-types, 51 FSM property tests, 6 WASM)
- **Event-Driven Coordination**: Message-based confirmation replaces arbitrary delays
- **Type-Safe Protocol IDs**: `DecoderId` and `FramerId` enums with compile-time exhaustiveness
- **Platform-Specific Optimization**: WASM uses `Rc<Cell<bool>>`, native tests use `Arc<AtomicBool>`
- **Code Quality**: Refactored probe_actor.rs (289-line function → 43 lines with reusable helpers)

### Removed

- **Device Swap Auto-Detection**: Eliminated false positives on multi-interface USB devices
- **Legacy ConnectionManager**: Replaced with actor system (~4000 LOC removed, ~7500 LOC added)

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

See `CLAUDE.md` for complete development workflow documentation.

**Key Principle**: All code must pass strict quality checks before merging:
- ✅ No unwrap/expect/panic
- ✅ No unsafe indexing
- ✅ Formatted with rustfmt
- ✅ Clean clippy with denied lints
