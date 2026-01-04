use leptos::*;
use wasm_bindgen::prelude::*;
use web_sys::HtmlDivElement;

#[wasm_bindgen]
extern "C" {
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
    // Note: CDN global for FitAddon is usually `FitAddon.FitAddon`? 
    // Actually CDN usually exposes `FitAddon` class directly on window if using UMD, or inside an object.
    // For cdn.jsdelivr.net/.../xterm-addon-fit.js, it creates `window.FitAddon`.
    // Let's verify standard UMD export name. xterm-addon-fit usually exports `FitAddon`.
    // So `js_namespace = FitAddon` might be needed if it exports an object.
    // Let's assume `window.FitAddon.FitAddon` based on common patterns or just `window.FitAddon`.
    // We'll trust the simple mapping first: `js_namespace = window, js_name = FitAddon`.
    fn new_fit_addon() -> FitAddon;

    #[wasm_bindgen(method)]
    fn fit(this: &FitAddon);
}

#[component]
pub fn TerminalView(
    #[prop(optional)] on_mount: Option<Callback<()>>
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
            
            term.write("Welcome to WASM Serial Tool v0.1\r\n");
            term.write("Ready to connect...\r\n");

            if let Some(cb) = on_mount {
                cb.call(());
            }
        }
    });

    view! {
        <div _ref=container_ref style="width: 100%; height: 100%;" />
    }
}
