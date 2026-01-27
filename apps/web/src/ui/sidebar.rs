use crate::actor_bridge::ActorBridge;
use core_types::DecoderId;
use leptos::*;

#[derive(Clone, Copy, PartialEq)]
pub enum ViewMode {
    Terminal,
    Hex,
    Mavlink,
}

#[component]
pub fn Sidebar(
    view_mode: Signal<ViewMode>,
    set_view_mode: WriteSignal<ViewMode>,
    manager: ActorBridge,
) -> impl IntoView {
    view! {
        <div style="width: 50px; background: rgb(25, 25, 25); display: flex; flex-direction: column; align-items: center; padding-top: 10px; border-left: 1px solid rgb(45, 45, 45);">
            // Terminal Button
            <button
                title="Terminal View (UTF-8)"
                style=move || format!(
                    "width: 40px; height: 40px; background: {}; color: white; border: none; cursor: pointer; border-radius: 4px; margin-bottom: 8px; display: flex; align-items: center; justify-content: center;",
                    if view_mode.get() == ViewMode::Terminal { "rgb(45, 45, 45)" } else { "transparent" }
                )
                on:click={
                    let m = manager.clone();
                    move |_| {
                        set_view_mode.set(ViewMode::Terminal);
                        m.set_decoder_typed(DecoderId::Utf8);
                    }
                }
            >
                {crate::xterm::icon()}
            </button>

            // Hex Inspector Button
            <button
                title="Hex Inspector (Hex List)"
                style=move || format!(
                    "width: 40px; height: 40px; background: {}; color: white; border: none; cursor: pointer; border-radius: 4px; margin-bottom: 8px; display: flex; align-items: center; justify-content: center;",
                    if view_mode.get() == ViewMode::Hex { "rgb(45, 45, 45)" } else { "transparent" }
                )
                on:click={
                    let m = manager.clone();
                    move |_| {
                        set_view_mode.set(ViewMode::Hex);
                        m.set_decoder_typed(DecoderId::Hex);
                    }
                }
            >
                 {crate::hex_view::icon()}
            </button>

            // MAVLink Button
            <button
                title="MAVLink Decoder"
                style=move || format!(
                    "width: 40px; height: 40px; background: {}; color: white; border: none; cursor: pointer; border-radius: 4px; margin-bottom: 8px; display: flex; align-items: center; justify-content: center; font-family: monospace; font-weight: bold; font-size: 0.8rem;",
                    if view_mode.get() == ViewMode::Mavlink { "rgb(45, 45, 45)" } else { "transparent" }
                )
                on:click={
                    let m = manager.clone();
                    move |_| {
                        set_view_mode.set(ViewMode::Mavlink);
                        m.set_decoder_typed(DecoderId::Mavlink);
                    }
                }
            >
                MAV
            </button>
        </div>
    }
}
