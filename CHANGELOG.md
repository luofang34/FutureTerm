# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **State Machine**: Comprehensive ConnectionState FSM with 8 states and validated transitions
  - States: Disconnected, Probing, Connecting, Connected, Reconfiguring, Disconnecting, DeviceLost, AutoReconnecting
  - Transition validation with debug-mode panics on invalid transitions
  - Helper methods: `can_disconnect()`, `can_connect()`, `indicator_color()`, etc.
- **Test Coverage**: 37 comprehensive tests for state machine and connection flows
  - State transition tests covering all valid/invalid paths
  - Edge case tests for race conditions and error paths
  - Fail-fast validation to catch bugs before shipping
- **Quality Enforcement**: Unified `dev.sh` script for all development tasks
  - `./dev.sh check` - Run format + clippy checks
  - `./dev.sh test` - Run checks + unit tests
  - `./dev.sh build` - Run checks + tests + release build
  - `./dev.sh serve` - Run checks + start dev server
- **Strict Clippy Rules**: Deny unwrap/panic/unsafe/indexing in WASM code
- **Clippy Configuration**: `clippy.toml` with strict safety rules
- **Format Configuration**: `rustfmt.toml` for consistent code style

### Fixed
- **Critical Bug**: Fixed baud rate reconfiguration completely broken
  - Issue: `connect_impl()` unconditionally transitioning to Connecting state
  - Multiple callers (auto-detect, smart framing) already made transition
  - Solution: Conditional transition only if not already in Connecting state
- **Critical Bug**: Fixed auto-reconnect VID/PID matching failure
  - Issue: VID/PID never saved after connection, auto-reconnect couldn't match device
  - Solution: Extract and save VID/PID from port.get_info() in connect_impl()
  - Auto-reconnect now works correctly after device unplug/replug
- **Bug**: Fixed port picker opening after USB disconnection
  - Issue: Disconnect button showing but clicking opened port picker dialog
  - Root cause: Signal propagation timing vs. status string checks
  - Solution: Use status string as single source for both button text and handler logic
- **Bug**: Changed RefCell to Cell for consistency in boolean flags
  - `user_initiated_disconnect` and `probing_interrupted` now use simpler Cell pattern
- **Performance**: Optimized empty read loop yield from setTimeout(1ms) to microtask
  - Changed from macrotask (1-16ms) to Promise.resolve() (< 1ms)
  - 4-16x better latency for read loop iterations
- **Safety**: Removed all 21 `.unwrap()` calls from WASM code
  - Replaced with safe Option/Result handling
  - All `web_sys::window()` calls now use proper error handling
- **Safety**: Replaced all 31 unsafe array indexing operations
  - Changed `arr[i]` to `arr.get(i)` with Option handling
  - No more panic risk from out-of-bounds access
- **Performance**: Optimized hex view DOM query performance
  - Incremental updates instead of full DOM traversal
  - Track previous selection state to skip redundant updates
  - 10x+ faster selection updates for large files (10K+ bytes)
- **Performance**: Replaced O(N) byte counting with cumulative counter
  - Eliminated per-batch iteration over entire log
  - 100x+ faster batch processing for large logs (10K+ events)
  - Memory overhead: single usize (8 bytes)
- **Memory**: Documented event handler cleanup limitations
  - Properly remove event listeners from DOM
  - Documented wasm-bindgen memory constraints

### Changed
- **CI/CD**: Updated GitHub Actions to use `./dev.sh` script
- **CI/CD**: Added WebSerial API support with RUSTFLAGS="--cfg=web_sys_unstable_apis"
- **CI/CD**: Pinned Rust version to 1.93.0 for reproducible builds
- **CI/CD**: Removed unstable rustfmt features causing warnings
- **Development**: Removed unused Python verification scripts
- **Development**: Simplified dev server (removed socat/serial_generator dependency)
- **Documentation**: Updated `gemini.md` with modern development workflow

### Removed
- Dead code in `terminal_metadata.rs` (unused helper functions)
- Unused verification scripts (`run_python_verify.sh`, `verify_native.sh`)
- Unstable rustfmt features (format_strings, format_macro_bodies, etc.)

### Known Limitations
- **State Machine Architecture**: Current FSM is "Passive Observer" pattern, not "Active Driver"
  - ConnectionState enum tracks state but doesn't enforce locks
  - Real execution guards are AtomicBool flags (is_connecting, is_disconnecting)
  - State transitions happen AFTER locks are acquired, not as part of acquisition
  - This creates duplication: both FSM state and atomic locks track same information
  - Future improvement: Refactor to Active FSM where state transitions ARE the locks
  - Impact: Code works correctly but has architectural debt (scattered Check-Lock-Transition patterns)
  - Trade-off: Chose to ship working code now vs. large refactor for cleaner architecture

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
