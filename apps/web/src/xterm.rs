use leptos::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::HtmlDivElement;

#[wasm_bindgen]
extern "C" {
    type Terminal;
    type FitAddon;

    #[wasm_bindgen(constructor, js_namespace = window)]
    fn new(options: &js_sys::Object) -> Terminal;

    #[wasm_bindgen(method)]
    fn open(this: &Terminal, parent: &HtmlDivElement);

    #[wasm_bindgen(method)]
    fn write(this: &Terminal, data: &str);

    #[wasm_bindgen(method, js_name = write)]
    fn write_u8(this: &Terminal, data: &[u8]);
    
    #[wasm_bindgen(method, js_name = loadAddon)]
    fn load_addon(this: &Terminal, addon: &FitAddon);

    #[wasm_bindgen(constructor, js_namespace = window, js_name = FitAddon_FitAddon)]
    fn new_fit_addon() -> FitAddon;

    #[wasm_bindgen(method, js_name = onData)]
    fn on_data(this: &Terminal, callback: &js_sys::Function) -> js_sys::Object; // returns IDisposable
}

#[derive(Clone)]
pub struct TerminalHandle(Terminal);

impl TerminalHandle {
    pub fn write_u8(&self, data: &[u8]) {
        self.0.write_u8(data);
    }
}

#[component]
pub fn TerminalView(
    #[prop(optional)] on_mount: Option<Callback<()>>,
    #[prop(optional)] on_data: Option<Callback<String>>,
    #[prop(optional)] on_terminal_ready: Option<Callback<TerminalHandle>>,
) -> impl IntoView 
{
    let container_ref = create_node_ref::<html::Div>();

    create_effect(move |_| {
        if let Some(div) = container_ref.get() {
            let options = js_sys::Object::new();
            let _ = js_sys::Reflect::set(&options, &"convertEol".into(), &JsValue::from(true));
            let term = Terminal::new(&options);
            term.open(&div);
            
            term.write("Welcome to WASM Serial Tool v0.1\r\n");
            term.write("Ready to connect...\r\n");

            // Input handling
            if let Some(cb) = on_data {
                let closure = Closure::wrap(Box::new(move |data: String| {
                    cb.call(data);
                }) as Box<dyn FnMut(String)>);
                term.on_data(closure.as_ref().unchecked_ref());
                closure.forget();
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
        <div _ref=container_ref style="width: 100%; height: 100%;" />
    }
}
