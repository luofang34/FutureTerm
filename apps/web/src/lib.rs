use leptos::*;

mod xterm;

#[component]
pub fn App() -> impl IntoView {
    let (terminal_ready, set_terminal_ready) = create_signal(false);

    view! {
        <div style="display: flex; flex-direction: column; height: 100vh;">
            <header style="padding: 10px; background: #333; color: white;">
                <h1>WASM Serial Tool</h1>
            </header>
            <main style="flex: 1; display: flex;">
                <div id="terminal-container" style="flex: 1; background: #000; overflow: hidden;">
                    <xterm::TerminalView on_mount=Callback::new(move |_| set_terminal_ready.set(true)) />
                </div>
                <div style="width: 300px; background: #eee; padding: 10px;">
                    <h3>Controls</h3>
                    <p>Status: {move || if terminal_ready.get() { "Ready" } else { "Initializing..." }}</p>
                </div>
            </main>
        </div>
    }
}
