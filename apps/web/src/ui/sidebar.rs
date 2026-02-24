use crate::context::AppContext;
use crate::views::{self, ViewId};
use leptos::*;

/// Sidebar component with view-switching buttons driven by the view registry.
///
/// Uses `AppContext` via Leptos context instead of receiving props.
#[component]
pub fn Sidebar() -> impl IntoView {
    let ctx = expect_context::<AppContext>();
    let view_mode = ctx.view_mode;
    let set_view_mode = ctx.set_view_mode;
    let manager = ctx.manager.clone();

    let buttons = views::all_views()
        .iter()
        .map(|desc| {
            let id = desc.id;
            let title = desc.title;
            let decoder = desc.decoder;
            let icon_fn = desc.icon;
            // MAVLink button needs extra font styling for inline text label
            let is_mavlink = id == ViewId::Mavlink;
            let m = manager.clone();

            view! {
                <button
                    title=title
                    style=move || {
                        let base = if is_mavlink {
                            "width: 40px; height: 40px; color: white; border: none; cursor: pointer; border-radius: 4px; margin-bottom: 8px; display: flex; align-items: center; justify-content: center; font-family: 'Menlo', 'Monaco', 'Consolas', 'Courier New', monospace; font-weight: bold; font-size: 0.8rem;"
                        } else {
                            "width: 40px; height: 40px; color: white; border: none; cursor: pointer; border-radius: 4px; margin-bottom: 8px; display: flex; align-items: center; justify-content: center;"
                        };
                        format!(
                            "{} background: {};",
                            base,
                            if view_mode.get() == id { "rgb(45, 45, 45)" } else { "transparent" }
                        )
                    }
                    on:click=move |_| {
                        set_view_mode.set(id);
                        m.set_decoder_typed(decoder);
                    }
                >
                    {icon_fn()}
                </button>
            }
        })
        .collect_view();

    view! {
        <div style="width: 50px; background: rgb(25, 25, 25); display: flex; flex-direction: column; align-items: center; padding-top: 10px; border-left: 1px solid rgb(45, 45, 45);">
            {buttons}
        </div>
    }
}
