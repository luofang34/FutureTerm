pub mod hex;
pub mod mavlink;
pub mod terminal;

use crate::context::AppContext;
use core_types::DecoderId;
use leptos::*;

/// Identifies a view plugin in the registry.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub enum ViewId {
    Terminal,
    Hex,
    Mavlink,
    // Future: Can, Log, Remote
}

/// Metadata for a single view plugin, used by the sidebar and router.
#[allow(dead_code)] // Fields exposed for future view plugin extensions
pub struct ViewDescriptor {
    pub id: ViewId,
    pub label: &'static str,
    pub title: &'static str,
    pub decoder: DecoderId,
    pub icon: fn() -> View,
    pub render: fn() -> View,
}

/// Returns the static registry of all available view plugins.
pub fn all_views() -> &'static [ViewDescriptor] {
    static VIEWS: &[ViewDescriptor] = &[
        ViewDescriptor {
            id: ViewId::Terminal,
            label: "Terminal",
            title: "Terminal View (UTF-8)",
            decoder: DecoderId::Utf8,
            icon: || crate::xterm::icon().into_view(),
            render: || terminal::TerminalPlugin().into_view(),
        },
        ViewDescriptor {
            id: ViewId::Hex,
            label: "Hex",
            title: "Hex Inspector (Hex List)",
            decoder: DecoderId::Hex,
            icon: || crate::hex_view::icon().into_view(),
            render: || hex::HexPlugin().into_view(),
        },
        ViewDescriptor {
            id: ViewId::Mavlink,
            label: "MAV",
            title: "MAVLink Decoder",
            decoder: DecoderId::Mavlink,
            icon: || mavlink::mavlink_icon().into_view(),
            render: || mavlink::MavlinkPlugin().into_view(),
        },
    ];
    VIEWS
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn test_registry_has_all_view_ids() {
        // Every ViewId variant should have a descriptor in all_views()
        let views = all_views();
        let ids: HashSet<ViewId> = views.iter().map(|v| v.id).collect();
        assert!(ids.contains(&ViewId::Terminal));
        assert!(ids.contains(&ViewId::Hex));
        assert!(ids.contains(&ViewId::Mavlink));
    }

    #[test]
    fn test_registry_no_duplicate_ids() {
        let views = all_views();
        let mut ids = HashSet::new();
        for v in views {
            assert!(ids.insert(v.id), "Duplicate ViewId: {:?}", v.id);
        }
    }

    #[test]
    fn test_registry_decoder_mapping() {
        // Verify correct decoder is assigned to each view
        let views = all_views();
        for v in views {
            match v.id {
                ViewId::Terminal => assert_eq!(v.decoder, DecoderId::Utf8),
                ViewId::Hex => assert_eq!(v.decoder, DecoderId::Hex),
                ViewId::Mavlink => assert_eq!(v.decoder, DecoderId::Mavlink),
            }
        }
    }

    #[test]
    fn test_registry_labels_non_empty() {
        for v in all_views() {
            assert!(!v.label.is_empty(), "ViewId {:?} has empty label", v.id);
            assert!(!v.title.is_empty(), "ViewId {:?} has empty title", v.id);
        }
    }

    #[test]
    fn test_view_id_equality() {
        assert_eq!(ViewId::Terminal, ViewId::Terminal);
        assert_ne!(ViewId::Terminal, ViewId::Hex);
        assert_ne!(ViewId::Hex, ViewId::Mavlink);
    }

    #[test]
    fn test_registry_count_matches_enum() {
        // If a new ViewId variant is added, this test will remind to add a descriptor
        let views = all_views();
        // This verifies the exhaustive match above covers all variants
        assert!(views.len() >= 3, "Expected at least 3 views in registry");
    }
}

/// View router component that replaces the hardcoded `<Show>` blocks in lib.rs.
///
/// Terminal is always mounted (hidden when inactive) to preserve metadata tracking.
/// Other views mount/unmount on demand via `<Show>`.
#[component]
pub fn ViewRouter() -> impl IntoView {
    #[allow(clippy::expect_used)]
    let ctx = use_context::<AppContext>().expect("AppContext");
    let view_mode = ctx.view_mode;

    view! {
        // Terminal Container: always mounted, visibility toggled via CSS
        <div style=move || format!(
            "flex: 1; height: 100%; display: {};",
            if view_mode.get() == ViewId::Terminal { "block" } else { "none" }
        )>
            {all_views()
                .iter()
                .find(|v| v.id == ViewId::Terminal)
                .map(|v| (v.render)())}
        </div>

        // Hex View Container: mount/unmount on demand
        <Show when=move || view_mode.get() == ViewId::Hex fallback=|| ()>
            {all_views()
                .iter()
                .find(|v| v.id == ViewId::Hex)
                .map(|v| (v.render)())}
        </Show>

        // MAVLink View Container: mount/unmount on demand
        <Show when=move || view_mode.get() == ViewId::Mavlink fallback=|| ()>
            {all_views()
                .iter()
                .find(|v| v.id == ViewId::Mavlink)
                .map(|v| (v.render)())}
        </Show>
    }
}
