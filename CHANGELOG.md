# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added
- **Quality Enforcement**: Unified `dev.sh` script for all development tasks
  - `./dev.sh check` - Run format + clippy checks
  - `./dev.sh test` - Run checks + unit tests
  - `./dev.sh build` - Run checks + tests + release build
  - `./dev.sh serve` - Run checks + start dev server
- **Strict Clippy Rules**: Deny unwrap/panic/unsafe/indexing in WASM code
- **Clippy Configuration**: `clippy.toml` with strict safety rules
- **Format Configuration**: `rustfmt.toml` for consistent code style

### Fixed
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
- **Development**: Removed unused Python verification scripts
- **Development**: Simplified dev server (removed socat/serial_generator dependency)
- **Documentation**: Updated `gemini.md` with modern development workflow

### Removed
- Dead code in `terminal_metadata.rs` (unused helper functions)
- Unused verification scripts (`run_python_verify.sh`, `verify_native.sh`)

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
