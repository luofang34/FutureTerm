use crate::context::AppContext;
use crate::xterm;
use leptos::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;

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

    let manager_tx = ctx.manager.clone();

    let on_terminal_mount = Callback::new(move |_| set_terminal_ready.set(true));

    let on_term_ready = Callback::from(move |t: xterm::TerminalHandle| {
        set_term_handle.set(Some(t.clone()));

        let mgr = manager_tx.clone();
        let on_data_cb = Closure::wrap(Box::new(move |data: JsValue| {
            if let Some(text) = data.as_string() {
                mgr.send_tx(text.into_bytes());
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
