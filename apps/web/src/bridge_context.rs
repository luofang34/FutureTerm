use crate::terminal_metadata::TerminalMetadata;
use actor_protocol::ConnectionState;
use leptos::*;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

/// Bridge-specific state for Safari/Firefox WebSocket transport.
///
/// Separated from AppContext so view plugins don't need to see bridge
/// internals. Only connect.rs, bridge.rs, dialogs.rs, and data_dispatch.rs
/// use this context.
#[derive(Clone)]
pub struct BridgeContext {
    pub active: Rc<Cell<bool>>,
    pub closing: Rc<Cell<bool>>,
    pub tx_queue: Rc<RefCell<Vec<Vec<u8>>>>,
    pub pending_baud: Rc<Cell<u32>>,
    pub ports: ReadSignal<Vec<(String, String)>>,
    pub set_ports: WriteSignal<Vec<(String, String)>>,
    pub port_pick: ReadSignal<Option<String>>,
    pub set_port_pick: WriteSignal<Option<String>>,
    pub ready: ReadSignal<Option<bool>>,
    pub set_ready: WriteSignal<Option<bool>>,
    pub show_install: ReadSignal<bool>,
    pub set_show_install: WriteSignal<bool>,
    pub needs_session_newline: Rc<Cell<bool>>,
}

/// Create bridge-specific context with all shared state for the WebSocket
/// bridge transport (Safari/Firefox fallback).
///
/// The `state_signal` and `terminal_metadata` params are needed for the
/// session-separator effect that sets `needs_session_newline` on reconnect.
pub fn create_bridge_context(
    state_signal: ReadSignal<ConnectionState>,
    terminal_metadata: ReadSignal<TerminalMetadata>,
) -> BridgeContext {
    let active: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    let closing: Rc<Cell<bool>> = Rc::new(Cell::new(false));
    let tx_queue: Rc<RefCell<Vec<Vec<u8>>>> = Rc::new(RefCell::new(Vec::new()));
    let pending_baud: Rc<Cell<u32>> = Rc::new(Cell::new(0));
    let (ports, set_ports) = create_signal::<Vec<(String, String)>>(Vec::new());
    let (port_pick, set_port_pick) = create_signal::<Option<String>>(None);
    let (ready, set_ready) = create_signal::<Option<bool>>(None);
    let (show_install, set_show_install) = create_signal(false);

    // Session separator flag
    let needs_session_newline: Rc<Cell<bool>> = Rc::new(Cell::new(false));

    // Set the session-newline flag when transitioning to Connected and the
    // terminal already has content from a previous session.
    {
        let flag = needs_session_newline.clone();
        create_effect(move |prev: Option<ConnectionState>| {
            let current = state_signal.get();
            if current == ConnectionState::Connected {
                if let Some(prev_state) = prev {
                    if prev_state != ConnectionState::Connected {
                        let meta = terminal_metadata.get_untracked();
                        if meta.has_content() {
                            flag.set(true);
                        }
                    }
                }
            }
            current
        });
    }

    BridgeContext {
        active,
        closing,
        tx_queue,
        pending_baud,
        ports,
        set_ports,
        port_pick,
        set_port_pick,
        ready,
        set_ready,
        show_install,
        set_show_install,
        needs_session_newline,
    }
}
