# Changelog

## [0.1.0] - 2025-01-20

### Added
- **MAVLink Support**: Full MAVLink v1/v2 decoding with auto-detection.
- **Auto-Baud**: Smart probing (passive first) for 1.5M/1M baud support.
- **Dashboard**: Specialized MAVLink telemetry view with Heartbeat/System tracking.
- **Safety**: Buffer overflow protection and DoS mitigation (50 system cap).
- **Performance**: Optimized rendering and event processing loops.
- **UI**: "FutureTerm" branding, Status Lights, and Dark Mode.

### Fixed
- **Reconnection**: Robust hot-unplug/replug detection.
- **Memory**: Capped limits for event buffers.
- **Parsing**: Strict CRC and Magic Byte validation.
