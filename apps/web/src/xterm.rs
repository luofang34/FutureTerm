use leptos::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::HtmlDivElement;

#[wasm_bindgen]
extern "C" {
    #[derive(Clone)]
    type Terminal;

    #[wasm_bindgen(constructor, js_namespace = window)]
    fn new() -> Terminal;

    #[wasm_bindgen(method)]
    fn open(this: &Terminal, parent: &HtmlDivElement);

    #[wasm_bindgen(method)]
    fn write(this: &Terminal, data: &str);

    #[wasm_bindgen(method)]
    fn onData(this: &Terminal, callback: &js_sys::Function) -> JsValue;

    #[wasm_bindgen(method)]
    fn loadAddon(this: &Terminal, addon: &JsValue);

    #[derive(Clone)]
    type FitAddon;

    #[wasm_bindgen(method)]
    fn fit(this: &FitAddon);
}

#[derive(Clone)]
pub struct TerminalHandle(Terminal);

impl TerminalHandle {
    pub fn write(&self, data: &str) {
        self.0.write(data);
    }
    
    pub fn on_data(&self, callback: js_sys::Function) {
        self.0.onData(&callback);
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
            // Configure Theme
            let opts = js_sys::Object::new();
            let theme = js_sys::Object::new();
            let _ = js_sys::Reflect::set(&theme, &"background".into(), &"#191919".into());
            let _ = js_sys::Reflect::set(&opts, &"theme".into(), &theme);
            
            // Fonts
            let _ = js_sys::Reflect::set(&opts, &"fontFamily".into(), &"Menlo, Monaco, 'Courier New', monospace".into());
            let _ = js_sys::Reflect::set(&opts, &"fontSize".into(), &JsValue::from(13));
            let _ = js_sys::Reflect::set(&opts, &"lineHeight".into(), &JsValue::from(1.2));
            
            // Cursor
            let _ = js_sys::Reflect::set(&opts, &"cursorBlink".into(), &JsValue::from(true));
            let _ = js_sys::Reflect::set(&opts, &"cursorStyle".into(), &"block".into());

            // Build Terminal using Reflect to pass options correctly
            let window = web_sys::window().unwrap();
            let term_ctor = js_sys::Reflect::get(&window, &"Terminal".into()).unwrap();
            let term_obj = js_sys::Reflect::construct(&term_ctor.unchecked_into(), &js_sys::Array::of1(&opts)).unwrap();
            let term: Terminal = term_obj.unchecked_into();
            
            // Dynamic FitAddon Loading
            let fit_addon = if let Ok(ns) = js_sys::Reflect::get(&window, &"FitAddon".into()) {
                if let Ok(ctor) = js_sys::Reflect::get(&ns, &"FitAddon".into()) {
                    if let Ok(func) = ctor.dyn_into::<js_sys::Function>() {
                         if let Ok(obj) = js_sys::Reflect::construct(&func, &js_sys::Array::new()) {
                             let addon: FitAddon = obj.unchecked_into();
                             term.loadAddon(addon.as_ref());
                             Some(addon)
                         } else { None }
                    } else { None }
                } else { None }
            } else { None };

            term.open(&div);
            
            // Initial Fit
            if let Some(ref addon) = fit_addon {
                addon.fit();
            }

            term.write("\x1b[1;36mFutureTerm v0.2\x1b[0m\r\n");
            term.write("----------------\r\n");
            term.write("Ready to connect...\r\n\r\n");

            // Resize Observer
            if let Some(addon) = fit_addon {
                let addon_clone = addon.clone();
                let closure = Closure::wrap(Box::new(move |_entries: js_sys::Array, _observer: JsValue| {
                    addon_clone.fit();
                }) as Box<dyn FnMut(js_sys::Array, JsValue)>);

                if let Ok(ro) = web_sys::ResizeObserver::new(closure.as_ref().unchecked_ref()) {
                    ro.observe(&div);
                    closure.forget(); 
                }
            }

            if let Some(cb) = on_terminal_ready {
                cb.call(TerminalHandle(term.clone().unchecked_into()));
            }

            if let Some(cb) = on_mount {
                cb.call(());
            }
        }
    });

    view! {
        <div style="width: 100%; height: 100%; padding: 8px; box-sizing: border-box; background: #191919; overflow: hidden;">
            <div _ref=container_ref style="width: 100%; height: 100%;" />
        </div>
    }
}
