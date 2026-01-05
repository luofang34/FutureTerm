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
    
    // Direct Terminal Handle (Bypassing Signals for Stream)
    let (term_handle, set_term_handle) = create_signal::<Option<xterm::TerminalHandle>>(None);

    // Effect to spawn worker on mount
    create_effect(move |_| {
        // Spawn worker
        // Note: Trunk usually names the worker file based on config. 
        // With data-type="worker", it might be "worker.js" or similar.
        // We'll try "worker.js" (standard trunk default for single worker or based on bin name).
        // If data-bin="worker", it should be "worker.js".
        // With data-type="worker", it might be "worker.js" or similar.
        // We'll try "worker.js" (standard trunk default for single worker or based on bin name).
        // If data-bin="worker", it should be "worker.js".
        if let Ok(w) = Worker::new("worker_bootstrap.js") {
            // Setup listener
            let cb = Closure::wrap(Box::new(move |e: MessageEvent| {
                 if let Ok(msg) = serde_wasm_bindgen::from_value::<WorkerToUi>(e.data()) {
                     match msg {
                         WorkerToUi::Status(s) => set_status.set(s),
                         WorkerToUi::DataBatch { frames, events: _ } => {
                             // Handle data - flatten frames and direct write
                             if let Some(term) = term_handle.get_untracked() {
                                 for f in frames {
                                     if !f.bytes.is_empty() {
                                         term.write_u8(&f.bytes);
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
          // Capture values before async
          let current_baud = baud_rate.get_untracked();
          spawn_local(async move {
              // Request Port
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
                      set_status.set("Port selected. Opening...".into());
                      
                      // 1. Open locally
                      let options = web_sys::SerialOptions::new(current_baud);
                      options.set_data_bits(8);
                      options.set_parity(web_sys::SerialParityType::None);
                      options.set_stop_bits(1);
                      options.set_flow_control(web_sys::SerialFlowControlType::None);
                      
                      let open_promise = port.open(&options);
                      
                      match wasm_bindgen_futures::JsFuture::from(open_promise).await {
                          Ok(_) => {
                              set_status.set("Port Open. Getting streams...".into());
                              
                              // 2. Get Streams
                              let readable = port.readable();
                              let writable = port.writable();
                              
                              // We need Readers/Writers, or just streams? 
                              // Current transport attach expects Reader/Writer.
                              // Let's create them here before transfer, OR transfer streams and let worker create readers?
                              // ReadableStream is transferable. DefaultReader is NOT?
                              // MDN: "ReadableStream, WritableStream, TransformStream" are transferable.
                              // We should transfer the STREAMS, and let the worker "get_reader()".
                              // BUT wait, transport.attach() takes Reader/Writer.
                              // Let's check transport code I just wrote: It takes Reader/Writer. 
                              // Are Reader/Writer transferable? SPEC says NO. Streams ARE.
                              // FIX: I need to update Worker logic to accept Streams and create Reader/Writer there?
                              // OR update main thread to create Reader/Writer?
                              // Usually Streams are transferred.
                              
                              // Correction: I should update worker logic to take ReadableStream/WritableStream 
                              // and call get_reader()/get_writer() inside the worker.
                              // However, I just updated transport to take Reader/Writer.
                              // Actually, I should update the worker logic to create reader/writer from the stream.
                              
                              // Let's assume for a second I send Streams.
                              // Refactoring lib.rs to send STREAMS.
                              
                              if let Some(w) = worker.get_untracked() {
                                  let msg = js_sys::Object::new();
                                  let _ = js_sys::Reflect::set(&msg, &"Connect".into(), &JsValue::from("true")); // Simple flag
                                  
                                  // We transfer the STREAMS.
                                  // But wait, to get reader, we need the stream.
                                  let readable_stream: web_sys::ReadableStream = readable.unchecked_into();
                                  let writable_stream: web_sys::WritableStream = writable.unchecked_into();
                                  
                                  // Lock them? No, transferring locks them.
                                  
                                  // I need to send the streams themselves.
                                  // But the worker expects Reader/Writer in my previous edit?
                                  // No, let's look at worker_logic.rs edit:
                                  // "let reader: ReadableStreamDefaultReader = r.unchecked_into();"
                                  // "t.attach(reader, writer);"
                                  // So the worker expects ALREADY CREATED Readers.
                                  // Can I transfer Readers?
                                  // "The ReadableStreamDefaultReader interface is NOT transferable."
                                  // CRITICIAL MISTAKE IN PREVIOUS STEP.
                                  // I must revert/fix worker_logic to accept STREAMS and create readers.
                                  
                                  // Ok, for this step, I will send STREAMS.
                                  // I will fix worker logic in next step if needed or assume I can fix it now.
                                  // Actually I can't fix worker logic in this tool call.
                                  // I will implement this to send STREAMS.
                                  // And I will simply cast them to Reader in worker (which will fail if I don't fix it).
                                  // Wait, I can fix worker in a subsequent step.
                                  
                                  let _ = js_sys::Reflect::set(&msg, &"readable".into(), &readable_stream);
                                  let _ = js_sys::Reflect::set(&msg, &"writable".into(), &writable_stream);

                                  let transfer = js_sys::Array::new();
                                  transfer.push(&readable_stream);
                                  transfer.push(&writable_stream);

                                  set_status.set(format!("Sending streams to worker..."));

                                  match w.post_message_with_transfer(&msg, &transfer) {
                                      Ok(_) => set_status.set("Streams sent. Connected.".into()),
                                      Err(e) => set_status.set(format!("PostMessage Failed: {:?}", e)),
                                  }
                              }
                          },
                          Err(e) => set_status.set(format!("Failed to open port: {:?}", e)),
                      }
                  },
                  Err(e) => {
                      let err_str = format!("{:?}", e);
                      if err_str.contains("NotFoundError") {
                          set_status.set("Connection cancelled (No port selected)".into());
                      } else if err_str.contains("NetworkError") {
                          set_status.set("Error: Port Busy! Unplug/Replug device & Refresh.".into());
                      } else {
                          set_status.set(format!("Error: {:?}", e));
                      }
                  },
              }
          });
     };

    let on_data = move |data: String| {
        if let Some(w) = worker.get_untracked() {
             // Debug 
             set_status.set(format!("TX: {} bytes", data.len()));
             
             let msg = UiToWorker::Send { data: data.into_bytes() };
             if let Ok(val) = serde_wasm_bindgen::to_value(&msg) {
                 let _ = w.post_message(&val);
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
                         if let Ok(data) = serde_wasm_bindgen::to_value(&msg) {
                             let _ = w.post_message(&data);
                         }
                    }
                }>
                    <option value="hex">Hex View</option>
                    <option value="nmea">NMEA</option>
                </select>

                <span style="font-family: monospace; font-size: 0.9rem;">{move || status.get()}</span>

                <button on:click=on_connect>
                   {move || format!("Connect ({})", baud_rate.get())}
                </button>
            </header>
            
            <div style="flex: 1; overflow: hidden;">
            <div style="flex: 1; overflow: hidden;">
                <xterm::TerminalView 
                    on_data=Callback::from(on_data) 
                    on_terminal_ready=Callback::from(move |t| set_term_handle.set(Some(t))) 
                />
            </div>
            </div>
        </div>
    }
}
