use leptos::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::{Worker, MessageEvent};
use crate::protocol::{UiToWorker, WorkerToUi};
use transport_webserial::WebSerialTransport;
use core_types::Transport;
use std::rc::Rc;
use std::cell::RefCell;
use wasm_bindgen_futures::spawn_local;

mod xterm;
pub mod protocol;
pub mod worker_logic;

#[component]
pub fn App() -> impl IntoView {
    let (_terminal_ready, set_terminal_ready) = create_signal(false);
    let (status, set_status) = create_signal("Idle".to_string());
    let (worker, set_worker) = create_signal::<Option<Worker>>(None);
    let (baud_rate, set_baud_rate) = create_signal(1500000);
    // Track connection state for toggle button
    let (connected, set_connected) = create_signal(false);
    
    // Transport - Main Thread Logic
    // Store in Rc<RefCell<Option<T>>> to share with closures
    let transport = Rc::new(RefCell::new(None::<WebSerialTransport>));
    
    // Direct Terminal Handle
    let (term_handle, set_term_handle) = create_signal::<Option<xterm::TerminalHandle>>(None);

    // Track parsed events
    let (events_list, set_events_list) = create_signal::<Vec<String>>(Vec::new());

    create_effect(move |_| {
        if let Ok(w) = Worker::new("worker_bootstrap.js") {
            // Restore TextDecoder for RX to Main Thread (if we ever want to decode locally? No, worker does that)
            // But wait, worker sends BACK a 'DataBatch' with frames. 
            // We need to print raw text to terminal.
            // The worker parses frames. Does it decode text? 
            // Looking at worker_logic.rs:
            // It receives IngestData -> Frames -> Decoder.
            // It sends back DataBatch { frames, events }.
            // Frames contain raw bytes.
            // So Main Thread needs to decode bytes to string for Xterm.
            
            let decoder = web_sys::TextDecoder::new().unwrap();
            let decode_opts = js_sys::Object::new();
            let _ = js_sys::Reflect::set(&decode_opts, &"stream".into(), &JsValue::from(true));
            let opts: web_sys::TextDecodeOptions = decode_opts.unchecked_into();

            let cb = Closure::wrap(Box::new(move |e: MessageEvent| {
                 if let Ok(msg) = serde_wasm_bindgen::from_value::<WorkerToUi>(e.data()) {
                     match msg {
                         WorkerToUi::Status(s) => {
                             // Ignore "Connected" from worker if it's just config confirmation
                             if !s.contains("Worker Ready") {
                                 set_status.set(s.clone());
                             }
                         },
                         WorkerToUi::DataBatch { frames, events } => {
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
                             
                             // Update events
                             if !events.is_empty() {
                                 set_events_list.update(|list| {
                                     for evt in events {
                                         // Keep last 50 events
                                         if list.len() >= 50 {
                                             list.remove(0);
                                         }
                                         // Format event
                                         let s = format!("{}: {}", evt.protocol, evt.summary);
                                         if !s.is_empty() {
                                            list.push(s);
                                         }
                                     }
                                 });
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

    let transport_clone = transport.clone();
    let transport_term = transport.clone();
    let on_connect = move |_| {
         // Toggle Logic
         if connected.get() {
             // Disconnect Logic
             let t_c = transport_clone.clone();
             spawn_local(async move {
                 // Retry loop to acquire lock (in case reading is active)
                 let mut closed = false;
                 for _ in 0..10 { // Try for 500ms
                     if let Ok(mut borrow) = t_c.try_borrow_mut() {
                         if let Some(mut t) = borrow.take() {
                             let _ = t.close().await;
                             set_status.set("Disconnected".into());
                             set_connected.set(false);
                             closed = true;
                             
                             // Notify worker to reset
                             if let Some(w) = worker.get_untracked() {
                                 let msg = UiToWorker::Disconnect;
                                 if let Ok(cmd_val) = serde_wasm_bindgen::to_value(&msg) {
                                     let envelope = js_sys::Object::new();
                                     let _ = js_sys::Reflect::set(&envelope, &"cmd".into(), &cmd_val);
                                     let _ = w.post_message(&envelope);
                                 }
                             }
                         }
                         break;
                     }
                     // Wait 50ms
                     let _ = wasm_bindgen_futures::JsFuture::from(
                          js_sys::Promise::new(&mut |r, _| {
                              let _ = web_sys::window().unwrap().set_timeout_with_callback_and_timeout_and_arguments_0(&r, 50);
                          })
                     ).await;
                 }
                 
                 if !closed {
                     set_status.set("Error: Could not close transport (Busy)".into());
                 }
             });
             return;
         }

         // Connect Logic
         let current_baud = baud_rate.get_untracked();
         let t_c = transport_clone.clone();
         
         spawn_local(async move {
             let nav = web_sys::window().unwrap().navigator();
             let serial = nav.serial();
             
             if serial.is_undefined() {
                 set_status.set("Error: WebSerial not supported.".into());
                 return;
             }

             // Request Port
             let options = js_sys::Object::new();
             let promise = match js_sys::Reflect::get(&serial, &"requestPort".into()) {
                 Ok(func_val) => {
                     let func: js_sys::Function = func_val.unchecked_into();
                     func.call1(&serial, &options).map_err(Into::into)
                 },
                 Err(e) => Err(e),
             };
             
             match promise {
                 Ok(p) => {
                     match wasm_bindgen_futures::JsFuture::from(js_sys::Promise::from(p)).await {
                         Ok(val) => {
                             let port: web_sys::SerialPort = val.unchecked_into();
                             set_status.set("Port selected. Connecting...".into());
                             
                             // Open Transport Locally
                             let mut t = WebSerialTransport::new();
                             match t.open(port, current_baud).await {
                                 Ok(_) => {
                                     set_status.set("Connected".into());
                                     set_connected.set(true);
                                     
                                     // Store transport
                                     if let Ok(mut borrow) = t_c.try_borrow_mut() {
                                         *borrow = Some(t);
                                     } else {
                                         set_status.set("Error: Could not store transport".into());
                                         return;
                                     }
                                     
                                     // Start Read Loop
                                     let t_loop = t_c.clone();
                                     spawn_local(async move {
                                         loop {
                                             let mut chunk = Vec::new();
                                             let mut ts = 0;
                                             let mut should_break = false;
                                             
                                             // 1. Borrow Shared to Read
                                             if let Ok(borrow) = t_loop.try_borrow() {
                                                 if let Some(t) = borrow.as_ref() {
                                                     if !t.is_open() { 
                                                         should_break = true; 
                                                     } else {
                                                         // This await holds the borrow!
                                                         match t.read_chunk().await {
                                                             Ok((d, t_val)) => { chunk = d; ts = t_val; },
                                                             Err(_) => { should_break = true; }
                                                         }
                                                     }
                                                 } else { should_break = true; }
                                             } else {
                                                  // Failed to borrow (Disconnect is mutating?)
                                                  should_break = true; 
                                             }

                                             if should_break { break; }
                                             
                                             // 2. Send to Worker
                                             if !chunk.is_empty() {
                                                 if let Some(w) = worker.get_untracked() {
                                                     let msg = UiToWorker::IngestData { data: chunk, timestamp_us: ts };
                                                     if let Ok(cmd_val) = serde_wasm_bindgen::to_value(&msg) {
                                                         let envelope = js_sys::Object::new();
                                                         let _ = js_sys::Reflect::set(&envelope, &"cmd".into(), &cmd_val);
                                                         let _ = w.post_message(&envelope);
                                                     }
                                                 }
                                             } else {
                                                 // Yield if no data (timeout occurred) to allow Disconnect to grab the lock
                                                 // read_chunk holds lock for ~100ms. We sleep 20ms to give a 20% window.
                                                 let _ = wasm_bindgen_futures::JsFuture::from(
                                                      js_sys::Promise::new(&mut |r, _| {
                                                          let _ = web_sys::window().unwrap().set_timeout_with_callback_and_timeout_and_arguments_0(&r, 20);
                                                      })
                                                 ).await;
                                             }
                                             
                                             // Loop continues. `read_chunk` yields internally via Promise.race
                                         }
                                         // Loop End
                                         set_connected.set(false);
                                     });
                                     
                                     // Send initial config to worker
                                     if let Some(w) = worker.get_untracked() {
                                         let msg = UiToWorker::Connect { baud_rate: current_baud };
                                         if let Ok(cmd_val) = serde_wasm_bindgen::to_value(&msg) {
                                             let envelope = js_sys::Object::new();
                                             let _ = js_sys::Reflect::set(&envelope, &"cmd".into(), &cmd_val);
                                             let _ = w.post_message(&envelope);
                                         }
                                     }
                                     
                                 },
                                 Err(e) => set_status.set(format!("Open Error: {:?}", e)),
                             }
                         },
                         Err(e) => set_status.set(format!("Connect Error: {:?}", e)),
                     }
                 },
                 Err(e) => set_status.set(format!("RequestPort Error: {:?}", e)),
             }
         });
    };

    view! {
        <div style="display: flex; flex-direction: column; height: 100vh; background: rgb(25, 25, 25); color: #eee;">
            <header style="padding: 10px; background: #1a1a1a; display: flex; align-items: center; gap: 20px; border-bottom: 1px solid #333;">
                <h1 style="margin: 0; font-size: 1.2rem; font-weight: 600;">FutureTerm</h1>
                <div style="flex: 1;"></div>
                
                <select 
                    style="background: #333; color: white; border: 1px solid #555; padding: 4px; border-radius: 4px;"
                    on:change=move |ev| {
                    let val = event_target_value(&ev);
                    if let Ok(b) = val.parse::<u32>() {
                        set_baud_rate.set(b);
                    }
                }>
                    <option value="9600">9600</option>
                    <option value="115200">115200</option>
                    <option value="1000000">1000000</option>
                    <option value="1500000" selected>1500000</option>
                </select>

                <select 
                    style="background: #333; color: white; border: 1px solid #555; padding: 4px; border-radius: 4px;"
                    on:change=move |ev| {
                    let val = event_target_value(&ev);
                     if let Some(w) = worker.get_untracked() {
                          let msg = UiToWorker::SetFramer { id: val };
                          let _ = w.post_message(&serde_wasm_bindgen::to_value(&msg).unwrap());
                     }
                }>
                    <option value="lines">Lines</option>
                    <option value="raw" selected>Raw</option>
                    <option value="cobs">COBS</option>
                    <option value="slip">SLIP</option>
                </select>

                <select 
                    style="background: #333; color: white; border: 1px solid #555; padding: 4px; border-radius: 4px;"
                    on:change=move |ev| {
                    let val = event_target_value(&ev);
                    if let Some(w) = worker.get_untracked() {
                         let msg = UiToWorker::SetDecoder { id: val };
                         let _ = w.post_message(&serde_wasm_bindgen::to_value(&msg).unwrap());
                    }
                }>
                    <option value="utf8">UTF-8</option>
                    <option value="nmea" selected>NMEA</option>
                    <option value="hex">Hex List</option>
                </select>

                <span style="font-size: 0.9rem; color: #aaa;">{move || status.get()}</span>
                <button 
                    style="padding: 5px 15px; background: #007acc; color: white; border: none; border-radius: 4px; cursor: pointer;"
                    on:click=on_connect>
                    {move || if connected.get() { "Disconnect" } else { "Connect" }}
                </button>
            </header>
            <main style="flex: 1; display: flex; overflow: hidden; height: 100%;">
                <div id="terminal-container" style="flex: 1; background: #191919; overflow: hidden; display: flex; flex-direction: column; min-width: 0;">
                    <xterm::TerminalView 
                        on_mount=Callback::new(move |_| set_terminal_ready.set(true)) 
                        on_terminal_ready=Callback::from(move |t: xterm::TerminalHandle| {
                            set_term_handle.set(Some(t.clone()));
                            
                            // Bind TX
                            let t_tx = transport_term.clone(); 
                            let on_data_cb = Closure::wrap(Box::new(move |data: JsValue| {
                                if let Some(text) = data.as_string() {
                                    let bytes = text.into_bytes();
                                    
                                    // Direct TX on Main Thread
                                    let t = t_tx.clone();
                                    spawn_local(async move {
                                        // Attempt to borrow transport
                                        // read_loop holds a shared borrow, so we can also take a shared borrow!
                                        if let Ok(borrow) = t.try_borrow() {
                                            if let Some(transport) = borrow.as_ref() {
                                                if transport.is_open() {
                                                     if let Err(e) = transport.write(&bytes).await {
                                                         web_sys::console::log_1(&format!("TX Error: {:?}", e).into());
                                                     }
                                                }
                                            }
                                        } else {
                                            web_sys::console::log_1(&"TX Dropped: Transport busy/locked".into());
                                        }
                                    });
                                }
                            }) as Box<dyn FnMut(JsValue)>);

                            t.on_data(on_data_cb.into_js_value().unchecked_into());
                        })
                    />
                </div>
                <div style="width: 250px; background: #222; border-left: 1px solid #444; color: #eee; overflow-y: auto; font-family: monospace; font-size: 0.8rem; padding: 5px;">
                     <div style="font-weight: bold; border-bottom: 1px solid #555; margin-bottom: 5px;">Decoded Events</div>
                     <ul>
                         <For
                             each=move || events_list.get()
                             key=|evt| evt.clone()
                             children=|evt| view! { <li>{evt}</li> }
                         />
                     </ul>
                </div>
            </main>
        </div>
    }
}
