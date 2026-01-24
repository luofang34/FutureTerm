# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [0.1.1] - 2026-01-23

### Added
- **Active FSM Driver**: Replaced passive state observation with an "Active Driver" FSM:
  - **Atomic Safety**: Unified state and lock into `AtomicConnectionState` (AtomicU8).
  - **CAS Transitions**: State transitions now act as explicit locks, preventing race conditions.
  - **Safety Helpers**: Added `finalize_connection` to prevent state/signal desynchronization.
- **Smart Probing v2**:
  - **Timeout Protection**: Added per-baud-rate timeouts to prevent hanging on silent devices.
  - **Clean Wakeup**: Reduced wakeup signal to single `\r` to prevent double prompts.
- **Buffer Hygiene**:
  - **Sanitization**: Added logic to strip leading junk/control characters from initial connection output, ensuring prompt alignment.
- **Property-Based Testing**: Added comprehensive property-based tests using `proptest` to verify FSM invariants:
  - State encoding round-trip verification
  - Lock bit preservation across operations
  - Transition validity under arbitrary sequences
  - Idempotence and determinism properties
- **Test Coverage**: Expanded test suite from 68 to 72 tests:
  - Added tests for disconnect race conditions and flag leak scenarios
  - Added tests for reconfigure VID/PID preservation behavior
  - Documents disconnect_internal() API usage patterns

### Fixed
- **Probing Hang**: Fixed infinite loop when probing silent devices by implementing a robust race-safe timeout.
- **Double Prompt**: Fixed duplicate command prompts caused by aggressive `\r\n\r\n` wakeup signals.
- **State Desync**: Fixed "Signal Lagging" error where atomic state and signals drifted apart during connection.
- **Disconnect Loop**: Fixed invalid `DeviceLost` state handling by allowing idempotent transitions.
- **UI Responsiveness**: Fixed "Disconnect" button being unresponsive during auto-reconnect loops.
- **Status Text Synchronization**: Ensured status text always reflects current FSM state:
  - Fixed status remaining "Connecting..." after successful connection
  - Removed all manual status overrides throughout codebase
  - FSM is now the single source of truth for all UI state
- **User Cancellation Flow**: Fixed invalid state transitions during user-initiated disconnect:
  - Added `Connecting` state to ensure proper transition sequence before port open
  - Removed invalid `Disconnected → Reconfiguring` transition
- **Code Review Fixes**: Addressed all 15 issues from comprehensive code review:
  - **HIGH Priority (2)**: Fixed auto-reconnect TOCTOU race and probing interruption state violations
  - **MEDIUM Priority (7)**: Systematic replacement of signal reads with atomic_state reads in critical paths
  - **LOW Priority (6)**: Code quality improvements including conditional DEBUG logs and constant documentation
- **Double-Disconnect Race**: Fixed flag leak when rapid disconnect attempts occur:
  - Second disconnect call now properly clears `user_initiated_disconnect` flag if guard acquisition fails
  - Prevents permanent connection failure after USB unplug during probing
- **Reconfigure VID/PID Preservation**: Fixed baud rate changes clearing auto-reconnect device info:
  - Added `disconnect_internal(clear_auto_reconnect: bool)` to control VID/PID clearing
  - User-initiated disconnect clears VID/PID, but reconfigure and auto-reconnect preserve it
- **Reconfigure Port Validation**: Removed incorrect port validity check that broke baud rate changes:
  - Port check was executing after intentional disconnect, always detecting port as invalid
  - Reconfigure now properly reconnects with new baud rate settings

### Changed
- **Modular Architecture**: Split `connection.rs` (3,700+ lines) into `types.rs`, `prober.rs`, `driver.rs`, and `reconnect.rs`.

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
