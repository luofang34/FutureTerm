# SPEC-0003: Framing + Decoder Plugin API

> **Status**: DRAFT
> **Version**: 1.1

## 1. Overview
We decouple **Framing** (how to split bytes) from **Decoding** (how to interpret frames).

## 2. Data Structures
... (Unchanged from v1.0)

## 3. Interfaces

### 3.1 Framer Trait
... (Unchanged from v1.0)

### 3.2 Decoder Trait
Responsible for parsing a complete `Frame` into semantic data.

```rust
trait Decoder {
    /// Attempt to parse a frame. Returns None if frame doesn't match protocol.
    fn ingest(&mut self, frame: &Frame) -> Option<DecodedEvent>;
}
```

**First-Wave Decoders (V1):**
1.  **NMEA**:
    *   Uses `nmea` crate.
    *   Parses standard sentences (GPGGA, GPRMC, etc.).
2.  **MAVLink**:
    *   Uses `mavlink` crate.
    *   Feature-gated dialects.
3.  **Hexdump** (Debug):
    *   Input: Any frame (usually from `RawStream` or `FixedSize`).
    *   Output: `DecodedEvent` where `summary` is the Hex string (`0A 0B ...`) and `fields` contains the ASCII representation.
    *   Visual: Renders in the structured table just like a protocol, useful for debugging unknown binary data.

## 4. Pipeline Logic
The Serial Worker runs the pipeline:
`Bytes -> Framer.push() -> [Frames] -> Decoder.ingest() -> [Events]`

*   **Hex View Mode**: When the user switches to "Hex View", we can either:
    *   Swap the Framer to a `FixedSizeFramer` (e.g., 16 bytes).
    *   Swap the Decoder to `HexDecoder`.
    *   This confirms the power of the decoupled architecture.
