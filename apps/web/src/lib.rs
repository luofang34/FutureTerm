use leptos::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Worker, MessageEvent};
use crate::protocol::{UiToWorker, WorkerToUi};

mod xterm;
pub mod protocol;
pub mod worker_logic;

#[component]
pub fn App() -> impl IntoView {
    let (terminal_ready, set_terminal_ready) = create_signal(false);
    let (status, set_status) = create_signal("Idle".to_string());
    let (worker, set_worker) = create_signal::<Option<Worker>>(None);
    let (baud_rate, set_baud_rate) = create_signal(115200);
    // Track connection state for toggle button
    let (connected, set_connected) = create_signal(false);
    
    // Direct Terminal Handle
    let (term_handle, set_term_handle) = create_signal::<Option<xterm::TerminalHandle>>(None);

    create_effect(move |_| {
        if let Ok(w) = Worker::new("worker_bootstrap.js") {
            // Restore TextDecoder for RX
            let decoder = web_sys::TextDecoder::new().unwrap();
            let decode_opts = js_sys::Object::new();
            let _ = js_sys::Reflect::set(&decode_opts, &"stream".into(), &JsValue::from(true));
            let opts: web_sys::TextDecodeOptions = decode_opts.unchecked_into();

            let cb = Closure::wrap(Box::new(move |e: MessageEvent| {
                 if let Ok(msg) = serde_wasm_bindgen::from_value::<WorkerToUi>(e.data()) {
                     match msg {
                         WorkerToUi::Status(s) => {
                             set_status.set(s.clone());
                             if s.contains("Connected") {
                                 set_connected.set(true);
                             } else if s.contains("Disconnected") {
                                 set_connected.set(false);
                             }
                         },
                         WorkerToUi::DataBatch { frames, events: _ } => {
                             if let Some(term) = term_handle.get_untracked() {
                                 for f in frames {
                                     if !f.bytes.is_empty() {
                                        if let Ok(text) = decoder.decode_with_u8_array_and_options(&f.bytes, &opts) {
                                            let text: String = text;
                                            if !text.is_empty() {
                                                term.write(&text);
                                            }
                                        }
                                     }
                                 }
                             }
                         }
                     }
                 }
            }) as Box<dyn FnMut(_)>);
            w.set_onmessage(Some(cb.as_ref().unchecked_ref()));
            cb.forget();
            
            set_worker.set(Some(w));
        } else {
            set_status.set("Failed to spawn worker".into());
        }
    });

    let on_connect = move |_| {
         // Toggle Logic
         if connected.get() {
             // Disconnect
             if let Some(w) = worker.get_untracked() {
                 let msg = UiToWorker::Disconnect;
                 if let Ok(cmd_val) = serde_wasm_bindgen::to_value(&msg) {
                     let envelope = js_sys::Object::new();
                     let _ = js_sys::Reflect::set(&envelope, &"cmd".into(), &cmd_val);
                     let _ = w.post_message(&envelope);
                 }
             }
             return;
         }

         // Connect Logic
         let current_baud = baud_rate.get_untracked();
         spawn_local(async move {
             let nav = web_sys::window().unwrap().navigator();
             let serial = nav.serial(); 
             
             let options = js_sys::Object::new();
             let promise = match js_sys::Reflect::get(&serial, &"requestPort".into()) {
                 Ok(func_val) => {
                     let func: js_sys::Function = func_val.unchecked_into();
                     func.call1(&serial, &options)
                 },
                 Err(_) => Err(js_sys::Error::new("requestPort not found").into()),
             }.map_err(|e| format!("{:?}", e));
             
             let promise_val = match promise {
                 Ok(p) => p,
                 Err(e) => { set_status.set(format!("Error: {:?}", e)); return; }
             };
             let promise: js_sys::Promise = promise_val.unchecked_into();
             match wasm_bindgen_futures::JsFuture::from(promise).await {
                 Ok(val) => {
                     let port: web_sys::SerialPort = val.unchecked_into();
                     set_status.set("Port selected. Connecting...".into());
                     
                     if let Some(w) = worker.get_untracked() {
                         let msg = UiToWorker::Connect { baud_rate: current_baud };
                         if let Ok(cmd_val) = serde_wasm_bindgen::to_value(&msg) {
                             let envelope = js_sys::Object::new();
                             let _ = js_sys::Reflect::set(&envelope, &"cmd".into(), &cmd_val);
                             let _ = js_sys::Reflect::set(&envelope, &"port".into(), &port);
                             
                             let transfer = js_sys::Array::new();
                             transfer.push(&port);
                             
                             let _ = w.post_message_with_transfer(&envelope, &transfer);
                         }
                     }
                 },
                 Err(e) => set_status.set(format!("Error: {:?}", e)),
             }
         });
    };

    let on_simulate = move |_| {
        if let Some(w) = worker.get_untracked() {
             set_status.set("Simulation requested...".into());
             let msg = UiToWorker::Simulate { duration_ms: 10000 };
             if let Ok(cmd_val) = serde_wasm_bindgen::to_value(&msg) {
                 let envelope = js_sys::Object::new();
                 let _ = js_sys::Reflect::set(&envelope, &"cmd".into(), &cmd_val);
                 let _ = w.post_message(&envelope);
             }
        }
    };

    view! {
        <div style="display: flex; flex-direction: column; height: 100vh;">
            <header style="padding: 10px; background: #333; color: white; display: flex; align-items: center; gap: 20px;">
                <h1 style="margin: 0; font-size: 1.2rem;">WASM Serial Tool</h1>
                <div style="flex: 1;"></div>
                
                <select on:change=move |ev| {
                    let val = event_target_value(&ev);
                    if let Ok(b) = val.parse::<u32>() {
                        set_baud_rate.set(b);
                    }
                }>
                    <option value="9600">9600</option>
                    <option value="115200" selected>115200</option>
                    <option value="921600">921600</option>
                    <option value="1500000">1500000</option>
                </select>

                <select on:change=move |ev| {
                    let val = event_target_value(&ev);
                    if let Some(w) = worker.get_untracked() {
                         let msg = UiToWorker::SetDecoder { id: val };
                         if let Ok(cmd_val) = serde_wasm_bindgen::to_value(&msg) {
                            let envelope = js_sys::Object::new();
                            let _ = js_sys::Reflect::set(&envelope, &"cmd".into(), &cmd_val);
                            let _ = w.post_message(&envelope);
                         }
                    }
                }>
                    <option value="hex">Hex View</option>
                    <option value="nmea">NMEA</option>
                </select>

                <span>{move || status.get()}</span>
                <button on:click=on_connect>
                    {move || if connected.get() { "Disconnect" } else { "Connect" }}
                </button>
                <button on:click=on_simulate style="margin-left: 10px; background: #666;">
                    "Simulate (10s)"
                </button>
            </header>
            <main style="flex: 1; display: flex;">
                <div id="terminal-container" style="flex: 1; background: #000; overflow: hidden;">
                    <xterm::TerminalView 
                        on_mount=Callback::new(move |_| set_terminal_ready.set(true)) 
                        on_terminal_ready=Callback::from(move |t| set_term_handle.set(Some(t)))
                    />
                </div>
            </main>
        </div>
    }
}
