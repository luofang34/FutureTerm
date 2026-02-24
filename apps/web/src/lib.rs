use leptos::*;
use wasm_bindgen_futures::spawn_local;
use web_sys::Worker;

// Actor system (replaces ConnectionManager)
mod actor_bridge;
mod actor_system;
use actor_bridge::ActorBridge;

mod bridge_context;
use bridge_context::create_bridge_context;

mod connect;
mod context;
use context::{create_app_context, AppContext};

mod data_dispatch;
mod dialogs;
mod header;

mod hex_view;
pub mod protocol;
mod terminal_metadata;
pub mod worker_logic;
mod xterm;

pub mod mavlink_view;
mod ui;
use ui::Sidebar;
mod views;
use views::ViewId;

#[component]
pub fn App() -> impl IntoView {
    // Actor System (replaces ConnectionManager)
    let manager_internal = actor_system::create_actor_system();
    // Worker signal must be created before ActorBridge (which reads it)
    let (worker, set_worker) = create_signal::<Option<Worker>>(None);
    let mut manager = ActorBridge::new(manager_internal, worker.into());

    // Create centralised application context (all shared signals)
    let ctx = create_app_context(manager.clone(), worker, set_worker);

    // Create bridge-specific context (Safari/Firefox WebSocket transport)
    let bctx = create_bridge_context(manager.state, ctx.terminal_metadata);

    // Inject bridge TX routing into ActorBridge so send_tx() works.
    // Must happen after create_bridge_context (which creates the Rc's).
    manager.set_bridge_tx(bctx.active.clone(), bctx.tx_queue.clone());
    // Update the manager inside ctx with the bridge-aware version
    let ctx = AppContext {
        manager: manager.clone(),
        ..ctx
    };
    provide_context(ctx.clone());
    provide_context(bctx.clone());

    // Local aliases for closures that capture individual Copy/Clone fields.
    // Header and dialog signals are now accessed via use_context in their
    // respective component modules (header.rs, dialogs.rs).
    // View-specific signals are accessed via AppContext in view plugins.
    let view_mode = ctx.view_mode;
    let set_view_mode = ctx.set_view_mode;
    let connected = ctx.connected;
    let baud_rate = ctx.baud_rate;
    let framing = ctx.framing;
    let active_framing = ctx.active_framing;
    let bridge_active_reconf = bctx.active.clone();
    let bridge_pending_baud_reconf = bctx.pending_baud.clone();

    // ── Startup pre-checks (bridge daemon probe for Safari/Firefox) ──
    connect::run_startup_precheck(&bctx);

    // Worker Logic (extracted to data_dispatch module)
    data_dispatch::setup_worker_dispatch(&ctx, &bctx);

    // Connect logic (extracted to connect module)
    let on_connect = {
        let ctx = ctx.clone();
        let bctx = bctx.clone();
        move |force_picker: bool| connect::on_connect(&ctx, &bctx, force_picker)
    };

    // --- Dynamic Reconfiguration Effect ---
    let manager_reconf = manager.clone();

    create_effect(move |_| {
        let b = baud_rate.get();
        let f = framing.get();
        let af = active_framing.get();

        if connected.get_untracked() {
            if bridge_active_reconf.get() {
                // Bridge mode: signal pending baud change; bridge loop applies it.
                // Only baud changes are supported; framing is handled by the worker.
                if b > 0 {
                    bridge_pending_baud_reconf.set(b);
                }
            } else {
                // WebSerial mode: use existing reconfigure path
                let manager_r = manager_reconf.clone();
                spawn_local(async move {
                    #[cfg(debug_assertions)]
                    web_sys::console::log_1(&"Dynamically Reconfiguring Port...".into());
                    manager_r.reconfigure(b, f, af);
                });
            }
        }
    });

    // Auto-Switch View to MAVLink Dashboard
    create_effect(move |_| {
        let dec = manager.decoder_id.get();
        if dec == "mavlink" && view_mode.get_untracked() != ViewId::Mavlink {
            set_view_mode.set(ViewId::Mavlink);
            // History now persists across decoder switches
        }
    });

    view! {
        <div style="display: flex; flex-direction: column; height: 100vh; background: rgb(25, 25, 25); color: #eee;">
            // Safari/Firefox bridge helper install dialog
            <dialogs::BridgeInstallDialog />
            // Bridge port picker dialog
            <dialogs::BridgePortPicker on_connect=on_connect.clone() />

            <header::Header on_connect=on_connect.clone() />
            <div style="flex: 1; display: flex; overflow: hidden; height: 100%; flex-direction: row;">
                <div style="flex: 1; position: relative; overflow: hidden; display: flex;">
                    <views::ViewRouter />
                </div>

                 // Sidebar (Moved to Right)
                 <Sidebar />
            </div>
        </div>
    }
}
