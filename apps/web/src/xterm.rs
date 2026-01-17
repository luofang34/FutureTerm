use leptos::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::HtmlDivElement;
use std::fmt;

#[wasm_bindgen]
extern "C" {
    // --- Terminal ---
    #[wasm_bindgen(extends = js_sys::Object)]
    pub type Terminal;

    #[wasm_bindgen(constructor, js_namespace = window)]
    pub fn new(options: Option<js_sys::Object>) -> Terminal;

    #[wasm_bindgen(method)]
    pub fn open(this: &Terminal, parent: &HtmlDivElement);

    #[wasm_bindgen(method)]
    pub fn write(this: &Terminal, data: &str);

    #[wasm_bindgen(method)]
    pub fn clear(this: &Terminal);

    #[wasm_bindgen(method, js_name = onData)]
    pub fn on_data(this: &Terminal, callback: js_sys::Function);

    // CHANGED: Accept JsValue for addon to support manual instantiation
    #[wasm_bindgen(method, js_name = loadAddon)]
    pub fn load_addon(this: &Terminal, addon: &JsValue);
}

// Manual Clone/PartialEq implementations
impl Clone for Terminal {
    fn clone(&self) -> Self {
        self.unchecked_ref::<JsValue>().clone().unchecked_into()
    }
}
impl PartialEq for Terminal {
    fn eq(&self, other: &Self) -> bool {
        self.unchecked_ref::<JsValue>() == other.unchecked_ref::<JsValue>()
    }
}

#[derive(Clone)]
pub struct TerminalHandle(pub Terminal);

impl fmt::Debug for TerminalHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TerminalHandle").finish()
    }
}

impl PartialEq for TerminalHandle {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl TerminalHandle {
    pub fn write(&self, data: &str) {
        self.0.write(data);
    }
    pub fn clear(&self) {
        self.0.clear();
    }
    pub fn on_data(&self, callback: js_sys::Function) {
        self.0.on_data(callback);
    }
}

// Helper to manually fit terminal using the addon instance
fn fit_terminal(addon: &JsValue) {
    if let Ok(fit_fn) = js_sys::Reflect::get(addon, &"fit".into()) {
        if let Ok(fit_fn) = fit_fn.dyn_into::<js_sys::Function>() {
            let _ = fit_fn.call0(addon);
        }
    }
}

#[component]
pub fn TerminalView(
    #[prop(optional)] on_mount: Option<Callback<()>>,
    #[prop(optional)] on_terminal_ready: Option<Callback<TerminalHandle>>,
) -> impl IntoView {
    let div_ref = create_node_ref::<html::Div>();

    create_effect(move |_| {
        if let Some(div) = div_ref.get() {
            // Options: Set Theme
            let options = js_sys::Object::new();
            let theme = js_sys::Object::new();
            // CHANGED: Match background to container (rgb(25,25,25) -> #191919)
            let _ = js_sys::Reflect::set(&theme, &"background".into(), &"#191919".into());
            let _ = js_sys::Reflect::set(&options, &"theme".into(), &theme);
            
            // Standard config
            let _ = js_sys::Reflect::set(&options, &"cursorBlink".into(), &true.into());
            let _ = js_sys::Reflect::set(&options, &"fontSize".into(), &14.into());
            let _ = js_sys::Reflect::set(&options, &"fontFamily".into(), &"Menlo, Monaco, 'Courier New', monospace".into());

            // Initialize xterm with options
            let term = Terminal::new(Some(options));
            
            // Initialize FitAddon manually via Reflection (bypassing wasm_bindgen macro issues)
            let mut fit_addon_instance: Option<JsValue> = None;
            
            if let Some(window) = web_sys::window() {
                // Access window.FitAddon (Object/Module)
                if let Ok(fa_module) = js_sys::Reflect::get(&window, &"FitAddon".into()) {
                    // Access window.FitAddon.FitAddon (Constructor)
                    if let Ok(fa_class) = js_sys::Reflect::get(&fa_module, &"FitAddon".into()) {
                        if let Ok(fa_ctor) = fa_class.dyn_into::<js_sys::Function>() {
                             if let Ok(instance) = js_sys::Reflect::construct(&fa_ctor, &js_sys::Array::new()) {
                                 term.load_addon(&instance);
                                 fit_addon_instance = Some(instance);
                             }
                        }
                    }
                }
            }

            // Convert Leptos HtmlElement to web_sys::HtmlDivElement
            // Clone the inner HtmlDivElement (via Deref) before casting
            let div_element: HtmlDivElement = <HtmlDivElement as Clone>::clone(&div).unchecked_into();
            
            term.open(&div_element);
            
            // Defer fit() to ensure layout is ready
            if let Some(fa) = fit_addon_instance {
                // Initial deferred fit
                let fa_clone1 = fa.clone();
                wasm_bindgen_futures::spawn_local(async move {
                     let _ = wasm_bindgen_futures::JsFuture::from(
                        js_sys::Promise::new(&mut |r, _| {
                             let _ = web_sys::window().unwrap().set_timeout_with_callback_and_timeout_and_arguments_0(&r, 10);
                        })
                    ).await;
                    fit_terminal(&fa_clone1);
                });
                
                // Add resize listener
                if let Some(window) = web_sys::window() {
                    let fa_clone2 = fa.clone();
                    let on_resize = Closure::wrap(Box::new(move || {
                        fit_terminal(&fa_clone2);
                    }) as Box<dyn FnMut()>);
                    
                    let _ = window.add_event_listener_with_callback("resize", on_resize.as_ref().unchecked_ref());
                    
                    // Cleanup listener when scope is dropped
                    on_cleanup(move || {
                         // Note: We need window here again. 
                         // To be perfectly safe we should clone window or look it up.
                         if let Some(w) = web_sys::window() {
                             let _ = w.remove_event_listener_with_callback("resize", on_resize.as_ref().unchecked_ref());
                         }
                    });
                }
            }
            
            if let Some(cb) = on_terminal_ready {
                 cb.call(TerminalHandle(term));
            }
            
            if let Some(cb) = on_mount {
                cb.call(());
            }
        }
    });

    view! {
        <div style="width: 100%; height: 100%; background: #191919; padding: 10px; box-sizing: border-box; position: relative;">
            <div _ref=div_ref style="width: 100%; height: 100%; overflow: hidden;" />
        </div>
    }
}

pub fn icon() -> impl IntoView {
    view! {
        <svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
           <rect x="3" y="3" width="18" height="18" rx="2" ry="2" />
           <path d="M8 8l4 4l-4 4" />
           <path d="M13 16h4" />
        </svg>
    }
}
