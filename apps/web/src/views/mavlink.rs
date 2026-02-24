use crate::context::AppContext;
use crate::mavlink_view;
use leptos::*;

/// Thin view-plugin wrapper around `mavlink_view::MavlinkView`.
///
/// Pulls all required signals from `AppContext` instead of receiving props.
#[component]
pub fn MavlinkPlugin() -> impl IntoView {
    let ctx = expect_context::<AppContext>();

    view! {
        <mavlink_view::MavlinkView events_list=ctx.events_list connected=ctx.connected />
    }
}

/// Inline icon for the MAVLink sidebar button (monospace text label).
pub fn mavlink_icon() -> impl IntoView {
    leptos::view! {
        <span style="font-family: 'Menlo', 'Monaco', 'Consolas', 'Courier New', monospace; font-weight: bold; font-size: 0.8rem;">
            MAV
        </span>
    }
}
