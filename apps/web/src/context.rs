use crate::actor_bridge::ActorBridge;
use crate::terminal_metadata::TerminalMetadata;
use crate::views::ViewId;
use crate::xterm::TerminalHandle;
use actor_protocol::ConnectionState;
use core_types::{DecodedEvent, RawEvent, SelectionRange};
use leptos::*;
use std::collections::VecDeque;
use web_sys::Worker;

/// Shared application state for the entire FutureTerm UI.
///
/// Holds ALL reactive signals and shared flags that were previously
/// scattered as local variables in `App()`. Passed via Leptos context
/// so child components can access it without prop-drilling.
///
/// Bridge-specific fields live in `BridgeContext` (bridge_context.rs).
#[derive(Clone)]
#[allow(dead_code)] // Fields exposed for future phases of the modular plugin refactor
pub struct AppContext {
    // ── Actor bridge (connection management) ──
    pub manager: ActorBridge,

    // ── Decoded events (MAVLink view) ──
    pub events_list: ReadSignal<VecDeque<DecodedEvent>>,
    pub set_events_list: WriteSignal<VecDeque<DecodedEvent>>,

    // ── Unified raw log (Hex view, Terminal metadata) ──
    pub raw_log: ReadSignal<VecDeque<RawEvent>>,
    pub set_raw_log: WriteSignal<VecDeque<RawEvent>>,
    pub raw_log_bytes: ReadSignal<usize>,
    pub set_raw_log_bytes: WriteSignal<usize>,
    pub hex_cursor: ReadSignal<usize>,
    pub set_hex_cursor: WriteSignal<usize>,

    // ── Cross-view selection sync ──
    pub global_selection: ReadSignal<Option<SelectionRange>>,
    pub set_global_selection: WriteSignal<Option<SelectionRange>>,

    // ── Terminal metadata (byte-position mapping) ──
    pub terminal_metadata: ReadSignal<TerminalMetadata>,
    pub set_terminal_metadata: WriteSignal<TerminalMetadata>,

    // ── Terminal handle ──
    pub term_handle: ReadSignal<Option<TerminalHandle>>,
    pub set_term_handle: WriteSignal<Option<TerminalHandle>>,

    // ── Baud rate / framing ──
    pub baud_rate: ReadSignal<u32>,
    pub set_baud_rate: WriteSignal<u32>,
    pub framing: ReadSignal<String>,
    pub set_framing: WriteSignal<String>,
    pub active_framing: ReadSignal<String>,
    pub set_active_framing: WriteSignal<String>,

    // ── Derived connected signal ──
    pub connected: Signal<bool>,

    // ── View mode ──
    pub view_mode: ReadSignal<ViewId>,
    pub set_view_mode: WriteSignal<ViewId>,

    // ── Worker ──
    pub worker: ReadSignal<Option<Worker>>,
    pub set_worker: WriteSignal<Option<Worker>>,

    // ── Terminal readiness ──
    pub _terminal_ready: ReadSignal<bool>,
    pub set_terminal_ready: WriteSignal<bool>,
}

/// Create all shared application state.
///
/// This replaces the dozens of individual `create_signal()` calls that
/// were previously at the top of `App()`.
///
/// The `worker` / `set_worker` signals are accepted as parameters because
/// they must be created *before* `ActorBridge::new()` (which reads them)
/// and then shared with the rest of the app via the context.
///
/// Bridge-specific state is created separately via `create_bridge_context()`.
pub fn create_app_context(
    manager: ActorBridge,
    worker: ReadSignal<Option<Worker>>,
    set_worker: WriteSignal<Option<Worker>>,
) -> AppContext {
    let (_terminal_ready, set_terminal_ready) = create_signal(false);
    let (view_mode, set_view_mode) = create_signal(ViewId::Terminal);

    // Derive connected signal from state machine
    let state_signal = manager.state;
    let connected = Signal::derive(move || state_signal.get() == ConnectionState::Connected);

    let (baud_rate, set_baud_rate) = create_signal(0u32);
    let (framing, set_framing) = create_signal("Auto".into());
    let (active_framing, set_active_framing) = create_signal("8N1".into());

    // Sync active_framing when detection completes
    let detected_framing = manager.detected_framing;
    create_effect(move |_| {
        let detected = detected_framing.get();
        if !detected.is_empty() {
            set_active_framing.set(detected);
        }
    });

    let (term_handle, set_term_handle) = create_signal::<Option<TerminalHandle>>(None);

    // Data architecture signals
    let (events_list, set_events_list) = create_signal::<VecDeque<DecodedEvent>>(VecDeque::new());
    let (raw_log, set_raw_log) = create_signal::<VecDeque<RawEvent>>(VecDeque::new());
    let (raw_log_bytes, set_raw_log_bytes) = create_signal(0usize);
    let (hex_cursor, set_hex_cursor) = create_signal(0usize);
    let (global_selection, set_global_selection) = create_signal::<Option<SelectionRange>>(None);
    let (terminal_metadata, set_terminal_metadata) = create_signal(TerminalMetadata::new());

    AppContext {
        manager,
        events_list,
        set_events_list,
        raw_log,
        set_raw_log,
        raw_log_bytes,
        set_raw_log_bytes,
        hex_cursor,
        set_hex_cursor,
        global_selection,
        set_global_selection,
        terminal_metadata,
        set_terminal_metadata,
        term_handle,
        set_term_handle,
        baud_rate,
        set_baud_rate,
        framing,
        set_framing,
        active_framing,
        set_active_framing,
        connected,
        view_mode,
        set_view_mode,
        worker,
        set_worker,
        _terminal_ready,
        set_terminal_ready,
    }
}
