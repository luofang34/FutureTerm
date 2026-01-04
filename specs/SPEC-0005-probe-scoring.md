# SPEC-0005: Auto-baud Probe + Scoring

> **Status**: DRAFT
> **Version**: 1.0

## 1. The Problem
Web Serial requires a `baudRate` to `open()`. We cannot passively listen to find the baud rate. We must actively `open()` -> `listen()` -> `close()` -> `repeat()`.

## 2. Probe Algorithm
User initiates "Auto-Detect":
1.  **Request Port**: User selects the device once.
2.  **Candidate List**: Common rates [9600, 19200, 38400, 57600, 115200, 230400, 460800, 921600].
3.  **Round Robin**:
    *   Open port at Candidate X.
    *   Wait for `T_probe` (e.g., 200ms) or `N_bytes` (e.g., 256 bytes).
    *   Read buffer.
    *   Close port.
    *   Compute **Score**.

## 3. Scoring Heuristics

### 3.1 Raw Text Score
*   **ASCII Printable Ratio**: What % of bytes are valid printable ASCII/UTF-8?
*   **Line Ending Consistency**: Do we see `\n` or `\r` appearing regularly?
*   **Gibberish Penalty**: High frequency of replacement characters () drastically lowers score.

### 3.2 Protocol Score (Decoder Assisted)
If a specific decoder is active (or all are probing):
*   **NMEA**: Checksums validity. `$GPGGA` patterns.
*   **MAVLink**: `0xFE` or `0xFD` magic bytes + valid CRC.

## 4. Result
*   Display "Confidence" level for top candidate.
*   If confidence > Threshold (e.g., 90%), auto-select.
*   Otherwise, present list to user: "Likely 115200 (60% confidence)".

## 5. UI Feedback
*   During probing, show a "Scanning... testing 115200..." progress indicator.
*   Warn user: "This sends DTR/RTS signals and may reset connected devices during open/close."
