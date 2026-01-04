# SPEC-0002: Transport Layer

> **Status**: DRAFT
> **Version**: 1.0

## 1. Overview
The Transport Layer abstracts the underlying serial hardware (Web Serial API vs Native Serial) behind a uniform interface. This allows the core logic (framing/decoding) and UI to remain agnostic to the runtime environment.

## 2. Interface Definition
The transport must satisfy the following trait (expressed in pseudo-Rust):

```rust
trait Transport {
    /// Connect to the port with specified settings.
    async fn open(&mut self, params: SerialParams) -> Result<()>;

    /// Close the connection.
    async fn close(&mut self) -> Result<()>;

    /// Read a chunk of bytes.
    /// Returns: Bytes + High-precision Timestamp (us)
    async fn read_chunk(&mut self) -> Result<(Vec<u8>, u64)>;

    /// Write bytes to the wire.
    async fn write(&mut self, data: &[u8]) -> Result<()>;

    /// Set control signals.
    async fn set_signals(&mut self, dtr: Option<bool>, rts: Option<bool>, break_signal: Option<bool>) -> Result<()>;
}
```

## 3. Implementations

### 3.1 Web Transport (`crates/transport-webserial`)
*   **Backing**: `web-sys::SerialPort` (Unstable API).
*   **Gate**: Gated behind `--cfg=web_sys_unstable_apis`.
*   **Constraints**:
    *   `open()` MUST explicitly provide `baudRate`.
    *   `read()` is obtained via `port.readable.getReader()`.
    *   `write()` is done via `port.writable.getWriter()`.
    *   Timestamps are derived from `performance.now()` at the moment of read in the Worker.

### 3.2 Native Transport (`crates/transport-native`)
*   **Backing**: `serialport` crate (Synchronous/Blocking).
*   **Wrapper**: Wrapped in a thread or `tokio-serial` for async compatibility if needed.
*   **Timestamps**: `std::time::SystemTime` or `Instant`.

## 4. Error Handling
*   Transports must normalize errors into a shared `TransportError` enum (e.g., `DeviceLost`, `AccessDenied`, `InvalidConfiguration`).
*   On `DeviceLost` (USB unplug), the transport should emit a robust error that allows the UI to enter a "Reconnecting" or "Disconnected" state without crashing.
