use crate::context::AppContext;
use crate::hex_view;
use leptos::*;

/// Thin view-plugin wrapper around `hex_view::HexView`.
///
/// Pulls all required signals from `AppContext` instead of receiving props.
#[component]
pub fn HexPlugin() -> impl IntoView {
    #[allow(clippy::expect_used)]
    let ctx = use_context::<AppContext>().expect("AppContext");

    view! {
        <hex_view::HexView
            raw_log=ctx.raw_log
            cursor=ctx.hex_cursor
            set_cursor=ctx.set_hex_cursor
            global_selection=ctx.global_selection
            set_global_selection=ctx.set_global_selection
        />
    }
}
