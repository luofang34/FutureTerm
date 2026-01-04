# SPEC-0004: UI + Performance Budgets

> **Status**: DRAFT
> **Version**: 1.1

## 1. Terminal Quality (Shell & Boot)
The raw console is the primary feature. It must feel like a native terminal.

*   **Engine**: `xterm.js` (standard for web terminals).
*   **Latency & "Feel"**:
    *   **Input Loop**: Keypress -> Web Worker -> Serial Write should happen immediately (0ms added delay). Do not wait for batching cycles for TX.
    *   **Output Loop**: RX -> Framer -> UI Batch. Small delays (16ms) are acceptable for high-throughput reads (prevent freezing), but low-throughput interactive echoes should arguably bypass heavy buffering.
*   **Memory Limit**: Hard scrollback limit to prevent OOM.

## 2. Productivity Features (V1)

### 2.1 Quick Actions (Snippets)
*   **UI**: A side/bottom panel with customizable buttons.
*   **Storage**: Browser `localStorage`. No cloud sync in v1.
*   **Data Model**: `{ id: string, label: string, command: string, appendNewline: boolean }`.
*   **Interaction**: Click sends data to Serial Worker immediately.

### 2.2 Hex Inspector View
*   **Implementation**: A "Virtual List" that renders `DecodedEvent`s from the `HexDecoder`.
*   **Layout**: Typical 3-column layout: `Offset | Hex Bytes (16) | ASCII`.
*   **Usage**: Users can switch the main view between "Terminal" (xterm.js) and "Inspector" (Virtual List).

## 3. Structured View Virtualization
... (Unchanged from v1.0)

## 4. Worker Communication
... (Unchanged from v1.0)
