use leptos::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::HtmlDivElement;

#[wasm_bindgen]
extern "C" {
    #[derive(Clone)]
    type Terminal;
    type FitAddon;

    #[wasm_bindgen(constructor, js_namespace = window)]
    fn new() -> Terminal;

    #[wasm_bindgen(method)]
    fn open(this: &Terminal, parent: &HtmlDivElement);

    #[wasm_bindgen(method)]
    fn write(this: &Terminal, data: &str);
    
    #[wasm_bindgen(method, js_name = loadAddon)]
    fn load_addon(this: &Terminal, addon: &FitAddon);

    #[wasm_bindgen(constructor, js_namespace = window, js_name = FitAddon_FitAddon)]
    fn new_fit_addon() -> FitAddon;

    #[wasm_bindgen(method)]
    fn fit(this: &FitAddon);
}

#[derive(Clone)]
pub struct TerminalHandle(Terminal);

impl TerminalHandle {
    pub fn write(&self, data: &str) {
        self.0.write(data);
    }
}

#[component]
pub fn TerminalView(
    #[prop(optional)] on_mount: Option<Callback<()>>,
    #[prop(optional)] on_terminal_ready: Option<Callback<TerminalHandle>>,
) -> impl IntoView 
{
    let container_ref = create_node_ref::<html::Div>();

    create_effect(move |_| {
        if let Some(div) = container_ref.get() {
            // Note: In real app, we need to ensure we don't double-initialize if effect re-runs.
            // But strict mode might trigger it twice.
            // xterm.js doesn't like being reopened on same div usually without disposal.
            // For V1 simple prototype, we just do it.

            let term = Terminal::new();
            // let fit_addon = FitAddon::new_fit_addon(); // Commented out until we verify namespace
            
            // term.load_addon(&fit_addon);
            term.open(&div);
            // fit_addon.fit();
            
            term.write("\x1b[1;36mFutureTerm v0.2\x1b[0m\r\n");
            term.write("----------------\r\n");
            term.write("Ready to connect...\r\n\r\n");

            if let Some(cb) = on_terminal_ready {
                cb.call(TerminalHandle(term.clone().unchecked_into()));
            }

            if let Some(cb) = on_mount {
                cb.call(());
            }
        }
    });

    view! {
        <div _ref=container_ref style="width: 100%; height: 100%;" />
    }
}
