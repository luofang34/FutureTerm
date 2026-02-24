use crate::context::AppContext;
use leptos::*;

/// Header toolbar containing baud rate, framing, framer dropdowns,
/// status indicator, RX/TX activity lights, and connect/disconnect button.
#[component]
pub fn Header(on_connect: impl Fn(bool) + 'static + Clone) -> impl IntoView {
    let ctx = expect_context::<AppContext>();

    let manager = ctx.manager.clone();
    let baud_rate = ctx.baud_rate;
    let set_baud_rate = ctx.set_baud_rate;
    let framing = ctx.framing;
    let set_framing = ctx.set_framing;

    let status = manager.get_status();
    let detected_baud = manager.detected_baud;
    let detected_framing = manager.detected_framing;

    let on_connect_arrow = on_connect.clone();

    view! {
        <header style="padding: 10px; background: rgb(25, 25, 25); display: flex; align-items: center; gap: 10px; border-bottom: 1px solid rgb(45, 45, 45);">
            <h1 style="margin: 0; font-family: 'Impact', 'Arial Black', sans-serif; font-style: italic; font-size: 1.5rem; font-weight: normal; letter-spacing: 1px;">FutureTerm</h1>
            <div style="flex: 1;"></div>

            <span style="font-size: 0.9rem; color: #aaa;">{move || status.get()}</span>

            <select
                style="width: 140px; background: #333; color: white; border: 1px solid #555; padding: 4px; border-radius: 4px;"
                on:change=move |ev| {
                let val = event_target_value(&ev);
                if let Ok(b) = val.parse::<u32>() {
                    set_baud_rate.set(b);
                }
            }
            prop:value=move || baud_rate.get().to_string()>
                <option value="0" selected=move || baud_rate.get() == 0>
                    {move || if baud_rate.get() == 0 && detected_baud.get() > 0 {
                        format!("Auto ({})", detected_baud.get())
                    } else {
                        "Auto Baudrate".to_string()
                    }}
                </option>
                <option value="9600">9600</option>
                <option value="19200">19200</option>
                <option value="38400">38400</option>
                <option value="57600">57600</option>
                <option value="115200">115200</option>
                <option value="230400">230400</option>
                <option value="460800">460800</option>
                <option value="500000">500000</option>
                <option value="921600">921600</option>
                <option value="1000000">1000000</option>
                <option value="1500000">1500000</option>
                <option value="2000000">2000000</option>
            </select>

            <select
                style="width: 110px; background: #333; color: white; border: 1px solid #555; padding: 4px; border-radius: 4px;"
                 on:change=move |ev| {
                      set_framing.set(event_target_value(&ev));
                 }
                 prop:value=move || framing.get()>
                <option value="Auto" selected=move || framing.get() == "Auto">
                    {move || if framing.get() == "Auto" && !detected_framing.get().is_empty() {
                        format!("Auto ({})", detected_framing.get())
                    } else {
                        "Auto Parity".to_string()
                    }}
                </option>
                <option value="8N1">8N1</option>
                <option value="8E1">8E1</option>
                <option value="8O1">8O1</option>
                <option value="7E1">7E1</option>
            </select>

            <select
                style="width: 80px; background: #333; color: white; border: 1px solid #555; padding: 4px; border-radius: 4px;"
                on:change={
                    let manager_framer = manager.clone();
                    move |ev| {
                        use core_types::FramerId;
                        use std::str::FromStr;
                        let val = event_target_value(&ev);
                        if let Ok(framer) = FramerId::from_str(&val) {
                            manager_framer.set_framer_typed(framer);
                        }
                    }
                }
            >
                <option value="lines">Lines</option>
                <option value="raw" selected>Raw</option>
                <option value="cobs">COBS</option>
                <option value="slip">SLIP</option>
            </select>

            // Status Light
            <div style=move || {
                let current_state = manager.state.get();
                let color = current_state.indicator_color();
                let animation = if current_state.indicator_should_pulse() {
                    "animation: pulse 0.3s ease-in-out infinite;"
                } else {
                    ""
                };

                format!("width: 12px; height: 12px; border-radius: 50%; background: {}; transition: background 0.3s ease; {}", color, animation)
            }></div>

            // RX/TX Indicators (Compact Stack)
            <div style="display: flex; flex-direction: column; align-items: flex-end; justify-content: center; gap: 2px;">
                // TX
                <div style="display: flex; align-items: center; gap: 6px; line-height: 1;">
                     <span style="font-family: sans-serif; font-size: 0.6rem; font-weight: bold; color: #ccc;">TX</span>
                     <div style=move || {
                         let active = manager.tx_active.get();
                         let (color, shadow) = if active {
                             ("rgb(80, 255, 80)", "0 0 4px rgb(80, 255, 80)")
                         } else {
                             ("rgb(60, 60, 60)", "none")
                         };
                         format!("width: 5px; height: 5px; border-radius: 50%; background: {}; box-shadow: {}; transition: background 0.05s;", color, shadow)
                     }></div>
                </div>
                // RX
                <div style="display: flex; align-items: center; gap: 6px; line-height: 1;">
                     <span style="font-family: sans-serif; font-size: 0.6rem; font-weight: bold; color: #ccc;">RX</span>
                     <div style=move || {
                         let active = manager.rx_active.get();
                         let (color, shadow) = if active {
                             ("rgb(255, 50, 50)", "0 0 4px rgb(255, 50, 50)")
                         } else {
                             ("rgb(60, 60, 60)", "none")
                         };
                         format!("width: 5px; height: 5px; border-radius: 50%; background: {}; box-shadow: {}; transition: background 0.05s;", color, shadow)
                     }></div>
                </div>
            </div>

            <style>
                {
                "@keyframes pulse {
                    0%, 100% { opacity: 1; }
                    50% { opacity: 0.4; }
                }
                .split-btn { transition: background-color 0.2s; }
                .split-btn:hover { background-color: #0062a3 !important; }
                .split-btn:active { background-color: #005a96 !important; }"
                }
            </style>
            <div style="display: flex; align-items: stretch; height: 28px; border-radius: 4px; overflow: hidden;">
                <button
                    class="split-btn"
                    style="padding: 0 12px; width: 100px; text-align: center; background: #007acc; color: white; border: none; cursor: pointer; font-size: 0.9rem; border-right: 1px solid rgba(255,255,255,0.2);"
                    title="Smart Connect (Auto-detects USB-Serial)"
                    on:click=move |_| on_connect(false)>
                    {move || {
                        if manager.state.get().button_shows_disconnect() {
                            "Disconnect"
                        } else {
                            "Connect"
                        }
                    }}
                </button>
                <button
                     class="split-btn"
                     style="width: 26px; background: #007acc; color: white; border: none; cursor: pointer; display: flex; align-items: center; justify-content: center; padding: 0;"
                     title="Manual Port Selection..."
                     on:click=move |_| on_connect_arrow(true)>
                    <svg width="10" height="10" viewBox="0 0 16 16" fill="currentColor" style="opacity: 0.9;">
                         <path d="M8 11L3 6h10l-5 5z"/>
                    </svg>
                </button>
            </div>
        </header>
    }
}
