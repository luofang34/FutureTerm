use crate::context::AppContext;
use crate::xterm;
use leptos::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use wasm_bindgen_futures::spawn_local;

/// Thin view-plugin wrapper around `xterm::TerminalView`.
///
/// Pulls all required signals from `AppContext` instead of receiving props.
/// Also wires up the terminal-ready and TX callbacks that were previously
/// inlined in `App()`.
#[component]
pub fn TerminalPlugin() -> impl IntoView {
    #[allow(clippy::expect_used)]
    let ctx = use_context::<AppContext>().expect("AppContext");

    let set_terminal_ready = ctx.set_terminal_ready;
    let set_term_handle = ctx.set_term_handle;
    let terminal_metadata = ctx.terminal_metadata;
    let global_selection = ctx.global_selection;
    let set_global_selection = ctx.set_global_selection;

    let manager_tx_cb = ctx.manager.clone();
    let bridge_active_term = ctx.bridge_active.clone();
    let bridge_tx_queue_term = ctx.bridge_tx_queue.clone();

    let on_terminal_mount = Callback::new(move |_| set_terminal_ready.set(true));

    let on_term_ready = Callback::from(move |t: xterm::TerminalHandle| {
        set_term_handle.set(Some(t.clone()));

        // Bind TX
        let manager_tx = manager_tx_cb.clone();
        let bridge_active_tx = bridge_active_term.clone();
        let bridge_tx_queue_tx = bridge_tx_queue_term.clone();
        let on_data_cb = Closure::wrap(Box::new(move |data: JsValue| {
            if let Some(text) = data.as_string() {
                let bytes = text.into_bytes();

                if bridge_active_tx.get() {
                    // Bridge mode - queue for WS send
                    #[cfg(debug_assertions)]
                    web_sys::console::log_1(
                        &format!("Bridge TX: queuing {} bytes", bytes.len()).into(),
                    );
                    bridge_tx_queue_tx.borrow_mut().push(bytes);
                } else {
                    // WebSerial mode
                    let active_manager = manager_tx.clone();
                    spawn_local(async move {
                        if let Err(e) = active_manager.write(&bytes).await {
                            #[cfg(debug_assertions)]
                            web_sys::console::log_1(&format!("TX Error: {:?}", e).into());
                        }
                    });
                }
            }
        }) as Box<dyn FnMut(JsValue)>);

        t.on_data(on_data_cb.into_js_value().unchecked_into());
    });

    view! {
        <xterm::TerminalView
            on_mount=on_terminal_mount
            on_terminal_ready=on_term_ready
            terminal_metadata=terminal_metadata
            global_selection=global_selection
            set_global_selection=set_global_selection
        />
    }
}
