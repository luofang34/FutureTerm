use crate::bridge_context::BridgeContext;
use leptos::*;

/// Modal dialog prompting the user to install the FutureTerm bridge helper
/// app for Safari/Firefox (browsers without WebSerial support).
#[component]
pub fn BridgeInstallDialog() -> impl IntoView {
    let bctx = expect_context::<BridgeContext>();

    let show_install = bctx.show_install;
    let set_show_install = bctx.set_show_install;

    view! {
        <Show when=move || show_install.get() fallback=|| ()>
            <div style="position: fixed; top: 0; left: 0; width: 100vw; height: 100vh; background: rgba(0,0,0,0.7); z-index: 10000; display: flex; align-items: center; justify-content: center;">
                <div style="background: #2a2a2a; border: 1px solid #555; border-radius: 8px; padding: 24px 32px; max-width: 480px; color: #eee; font-family: sans-serif;">
                    <h2 style="margin: 0 0 12px; font-size: 1.2rem; color: #ff9800;">"Serial Port Helper Required"</h2>
                    <p style="margin: 0 0 8px; font-size: 0.9rem; line-height: 1.5; color: #ccc;">
                        "Your browser doesn\u{2019}t support the WebSerial API. FutureTerm needs a small helper app running locally to access your serial ports."
                    </p>
                    <p style="margin: 0 0 16px; font-size: 0.9rem; line-height: 1.5; color: #ccc;">
                        "The helper is lightweight (~1 MB), runs only when needed, and shuts down automatically after 2 minutes of inactivity."
                    </p>
                    <div style="display: flex; gap: 12px; justify-content: flex-end; align-items: center;">
                        <button
                            style="padding: 8px 16px; background: #444; color: #ccc; border: 1px solid #666; border-radius: 4px; cursor: pointer; font-size: 0.9rem; line-height: 1.4;"
                            on:click=move |_| set_show_install.set(false)>
                            "Cancel"
                        </button>
                        <a
                            href="/bridge-helper"
                            target="_blank"
                            style="padding: 8px 16px; background: #007acc; color: white; border: 1px solid #007acc; border-radius: 4px; cursor: pointer; font-size: 0.9rem; line-height: 1.4; text-decoration: none; display: inline-block;">
                            "Download Helper"
                        </a>
                    </div>
                </div>
            </div>
        </Show>
    }
}

/// Modal dialog listing available serial ports discovered by the bridge
/// daemon. The user picks one (or cancels) to continue the connection flow.
#[component]
pub fn BridgePortPicker(on_connect: impl Fn(bool) + 'static + Clone) -> impl IntoView {
    // Suppress unused-variable warning -- on_connect is reserved for future
    // use when the port-picker may trigger a connect after selection.
    let _ = &on_connect;

    let bctx = expect_context::<BridgeContext>();

    let ports = bctx.ports;
    let set_port_pick = bctx.set_port_pick;

    view! {
        <Show when=move || !ports.get().is_empty() fallback=|| ()>
            <div style="position: fixed; top: 0; left: 0; width: 100vw; height: 100vh; background: rgba(0,0,0,0.7); z-index: 10000; display: flex; align-items: center; justify-content: center;">
                <div style="background: #2a2a2a; border: 1px solid #555; border-radius: 8px; padding: 24px 32px; max-width: 480px; min-width: 320px; color: #eee; font-family: sans-serif;">
                    <h2 style="margin: 0 0 16px; font-size: 1.2rem;">"Select Serial Port"</h2>
                    {move || {
                        ports.get().into_iter().map(|(path, desc)| {
                            let path_click = path.clone();
                            view! {
                                <button
                                    style="display: block; width: 100%; padding: 10px 16px; margin: 4px 0; background: #333; color: #eee; border: 1px solid #555; border-radius: 4px; cursor: pointer; text-align: left; font-size: 0.9rem;"
                                    on:click=move |_| set_port_pick.set(Some(path_click.clone()))>
                                    {desc}
                                </button>
                            }
                        }).collect_view()
                    }}
                    <button
                        style="display: block; width: 100%; padding: 8px 16px; margin-top: 12px; background: #444; color: #ccc; border: 1px solid #666; border-radius: 4px; cursor: pointer; font-size: 0.9rem;"
                        on:click=move |_| set_port_pick.set(Some(String::new()))>
                        "Cancel"
                    </button>
                </div>
            </div>
        </Show>
    }
}
