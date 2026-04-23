use core_types::{SelectionRange, SelectionSource};
use leptos::*;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;

use super::layout::SelectionOrigin;

/// Installs an effect that updates hex/ascii span highlight classes in response
/// to global selection and active-origin changes.
///
/// Uses requestAnimationFrame to batch DOM updates and compares against the
/// previous selection to skip no-op work.
pub(super) fn setup_highlight_effect(
    global_selection: Option<ReadSignal<Option<SelectionRange>>>,
    active_origin: ReadSignal<Option<SelectionOrigin>>,
) {
    let (prev_selection, set_prev_selection) =
        create_signal::<Option<(usize, usize, SelectionSource)>>(None);

    create_effect(move |_| {
        let range_opt = global_selection.and_then(|g| g.get());
        let origin = active_origin.get();
        let prev = prev_selection.get();

        // Use requestAnimationFrame to batch DOM updates
        let set_prev = set_prev_selection;
        let callback = Closure::once(Box::new(move || {
            if let Some(window) = web_sys::window() {
                if let Some(document) = window.document() {
                    // Convert current selection to comparable tuple
                    let current = range_opt
                        .as_ref()
                        .map(|r| (r.start_byte_offset, r.end_byte_offset, r.source_view));

                    // Only update if selection actually changed
                    if prev == current {
                        return;
                    }

                    // Clear all highlight classes unconditionally before reapplying.
                    // This ensures stale classes from previous origin changes are removed
                    // (e.g., switching from Hex→Ascii selection would leave bg-sync on
                    // hex-column elements that overlap the new range).
                    if let Ok(elements) =
                        document.query_selector_all(".hex-byte.bg-sync, .hex-byte.bg-term")
                    {
                        for i in 0..elements.length() {
                            if let Some(el) = elements.get(i) {
                                if let Some(el) = el.dyn_ref::<web_sys::HtmlElement>() {
                                    let _ = el.class_list().remove_2("bg-sync", "bg-term");
                                }
                            }
                        }
                    }

                    // Apply new highlights if selection exists
                    if let Some(range) = range_opt {
                        let is_terminal = range.source_view == SelectionSource::Terminal;
                        let is_hex_view = range.source_view == SelectionSource::HexView;

                        // Query only elements in range for better performance
                        if let Ok(elements) = document.query_selector_all(".hex-byte[data-offset]")
                        {
                            for i in 0..elements.length() {
                                if let Some(el) = elements.get(i) {
                                    if let Some(el) = el.dyn_ref::<web_sys::HtmlElement>() {
                                        if let Some(offset_str) = el.dataset().get("offset") {
                                            if let Ok(offset) = offset_str.parse::<usize>() {
                                                if range.contains_offset(offset) {
                                                    // Terminal selection: both hex and ASCII get
                                                    // bg-term
                                                    if is_terminal {
                                                        let _ = el.class_list().add_1("bg-term");
                                                    }
                                                    // HexView selection: sync highlighting logic
                                                    else if is_hex_view {
                                                        let is_ascii =
                                                            el.class_list().contains("ascii-char");
                                                        // If origin is ASCII and this is hex, or
                                                        // vice versa, apply sync color
                                                        if (origin == Some(SelectionOrigin::Ascii)
                                                            && !is_ascii)
                                                            || (origin
                                                                == Some(SelectionOrigin::Hex)
                                                                && is_ascii)
                                                        {
                                                            let _ =
                                                                el.class_list().add_1("bg-sync");
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }

                    // Update previous selection for next comparison
                    set_prev.set(current);
                }
            }
        }) as Box<dyn FnOnce()>);

        if let Some(window) = web_sys::window() {
            let _ = window.request_animation_frame(callback.as_ref().unchecked_ref());
            // For FnOnce callbacks in requestAnimationFrame, we need to forget
            // because they execute once and then should be cleaned up by the browser.
            // This is a known limitation in wasm-bindgen for one-shot callbacks.
            callback.forget();
        }
    });
}
