use core_types::{SelectionRange, SelectionSource};
use leptos::*;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;

use super::layout::{HexRow, SelectionOrigin};

/// Returns an `on:copy` handler that reads the current browser selection,
/// maps each selected span back to a byte offset via data attributes, and
/// writes a hex-or-ASCII string into the clipboard.
///
/// The handler captures `all_hex_rows` so it can rebuild the string from
/// the raw byte log rather than from rendered DOM text.
pub(super) fn make_copy_handler(
    all_hex_rows: Memo<Vec<HexRow>>,
) -> impl Fn(web_sys::Event) + 'static {
    move |ev: web_sys::Event| {
        let Some(ev) = ev.dyn_into::<web_sys::ClipboardEvent>().ok() else {
            return;
        };
        let Some(window) = web_sys::window() else {
            return;
        };
        let Some(selection) = window.get_selection().ok().flatten() else {
            return;
        };
        if selection.is_collapsed() {
            return;
        }

        let anchor_node = selection.anchor_node();
        let focus_node = selection.focus_node();

        let (Some(anchor), Some(focus)) = (anchor_node, focus_node) else {
            return;
        };

        // Helper to determine component type and offset
        // Returns: (offset, is_ascii)
        let get_info = |node: web_sys::Node| -> Option<(usize, bool)> {
            let mut curr = Some(node);
            let mut is_ascii = None;
            let mut precise_offset = None;
            let mut row_offset = None;

            while let Some(n) = curr {
                if let Some(el) = n.dyn_ref::<web_sys::HtmlElement>() {
                    // Check for specific byte offset
                    if precise_offset.is_none() {
                        if let Some(off) = el.dataset().get("offset") {
                            if let Ok(val) = off.parse::<usize>() {
                                precise_offset = Some(val);
                            }
                        }
                    }

                    // Check for row offset
                    if row_offset.is_none() {
                        if let Some(off) = el.dataset().get("row-offset") {
                            if let Ok(val) = off.parse::<usize>() {
                                row_offset = Some(val);
                            }
                        }
                    }

                    // Check container type
                    if el.class_list().contains("ascii-container") {
                        is_ascii = Some(true);
                    } else if el.class_list().contains("hex-data-container") {
                        is_ascii = Some(false);
                    }
                }
                curr = n.parent_element().map(|e| e.into());
            }

            match (is_ascii, precise_offset, row_offset) {
                (Some(ascii), Some(p_off), _) => Some((p_off, ascii)),
                (Some(ascii), None, Some(r_off)) => Some((r_off, ascii)), // Fallback to row start
                _ => None,
            }
        };

        let start_info = get_info(anchor);
        let end_info = get_info(focus);

        let (Some((start_off, start_ascii)), Some((end_off, _))) = (start_info, end_info) else {
            return;
        };

        let (min, max) = if start_off < end_off {
            (start_off, end_off)
        } else {
            (end_off, start_off)
        };

        let mut content = String::new();
        let rows = all_hex_rows.get();

        for row in rows {
            if row.offset + row.bytes.len() <= min {
                continue;
            }
            if row.offset > max {
                break;
            }

            for (i, &b) in row.bytes.iter().enumerate() {
                let abs_off = row.offset + i;
                if abs_off >= min && abs_off <= max {
                    if start_ascii {
                        if (32..=126).contains(&b) {
                            content.push(b as char);
                        } else {
                            content.push('.');
                        }
                    } else {
                        if !content.is_empty() && content.len() % 3 == 2 {
                            content.push(' ');
                        } else if !content.is_empty() && content.ends_with('\n') {
                            /* Newline, no space needed */
                        } else if !content.is_empty() {
                            content.push(' ');
                        }

                        content.push_str(&format!("{:02X}", b));
                    }
                }
            }
        }

        if let Some(clipboard_data) = ev.clipboard_data() {
            let _ = clipboard_data.set_data("text/plain", &content);
            ev.prevent_default();
        }
    }
}

/// Installs a document-level `selectionchange` listener that reflects native
/// browser selection into the global selection signal, bucketed by origin
/// (hex column vs ASCII column).
///
/// The listener is throttled to 60fps and uses data-offset attributes on
/// rendered hex/ascii spans to map DOM selection back to byte offsets.
pub(super) fn setup_selection_change_listener(
    set_active_origin: WriteSignal<Option<SelectionOrigin>>,
    set_global_selection: Option<WriteSignal<Option<SelectionRange>>>,
) {
    let (last_selection_time, set_last_selection_time) = create_signal(0.0);

    create_effect(move |_| {
        let set_global = set_global_selection;
        let last_time = last_selection_time;
        let set_last = set_last_selection_time;

        let callback = Closure::wrap(Box::new(move || {
            // Throttle to 60fps (16.67ms) for performance
            let now = js_sys::Date::now();
            let last = last_time.get_untracked();
            if now - last < 16.67 {
                return;
            }
            set_last.set(now);

            if let Some(window) = web_sys::window() {
                if let Some(selection) = window.get_selection().ok().flatten() {
                    // If no valid selection, clear global selection
                    if selection.is_collapsed() {
                        set_active_origin.set(None);
                        if let Some(set_g) = set_global {
                            set_g.set(None);
                        }
                        return;
                    }

                    let anchor_node = selection.anchor_node();
                    let focus_node = selection.focus_node();

                    if let (Some(anchor), Some(focus)) = (anchor_node, focus_node) {
                        let get_info = |node: web_sys::Node| -> Option<(usize, bool)> {
                            let mut curr = Some(node);
                            let mut offset_found = None;
                            let mut is_ascii = None;

                            while let Some(n) = curr {
                                if let Some(el) = n.dyn_ref::<web_sys::HtmlElement>() {
                                    if offset_found.is_none() {
                                        if let Some(off) = el.dataset().get("offset") {
                                            if let Ok(val) = off.parse::<usize>() {
                                                offset_found = Some(val);
                                            }
                                        }
                                    }
                                    // Check container class for reliable column detection
                                    if is_ascii.is_none() {
                                        if el.class_list().contains("ascii-container") {
                                            is_ascii = Some(true);
                                        } else if el.class_list().contains("hex-data-container") {
                                            is_ascii = Some(false);
                                        }
                                    }
                                    if offset_found.is_some() && is_ascii.is_some() {
                                        break;
                                    }
                                }
                                curr = n.parent_element().map(|e| e.into());
                            }

                            // Require precise byte offset — no fallback to row offset
                            // to prevent full-row expansion when anchor/focus lands on
                            // a container element
                            match (is_ascii, offset_found) {
                                (Some(ascii), Some(off)) => Some((off, ascii)),
                                _ => None,
                            }
                        };

                        let start_info = get_info(anchor.clone());
                        let end_info = get_info(focus);

                        if let (Some((start_off, start_is_ascii)), Some((end_off, end_is_ascii))) =
                            (start_info, end_info)
                        {
                            // Cross-column detected. Clamp focus to the anchor's column.
                            if start_is_ascii != end_is_ascii {
                                let container_class = if start_is_ascii {
                                    "ascii-container"
                                } else {
                                    "hex-data-container"
                                };
                                let selector = format!(
                                    ".{} .hex-byte[data-offset=\"{}\"]",
                                    container_class, end_off
                                );
                                if let Some(document) = window.document() {
                                    if let Ok(Some(target_el)) = document.query_selector(&selector)
                                    {
                                        if let Some(text_node) = target_el.first_child() {
                                            let focus_offset = if start_off <= end_off {
                                                target_el
                                                    .text_content()
                                                    .map(|t| t.len() as u32)
                                                    .unwrap_or(1)
                                            } else {
                                                0
                                            };
                                            let _ = selection.set_base_and_extent(
                                                &anchor,
                                                selection.anchor_offset(),
                                                &text_node,
                                                focus_offset,
                                            );
                                        }
                                    }
                                }

                                // Process the clamped range inline to avoid throttle swallowing re-fired event
                                let (min, max) = if start_off < end_off {
                                    (start_off, end_off)
                                } else {
                                    (end_off, start_off)
                                };
                                let max_exclusive = max + 1;

                                if min < max_exclusive {
                                    set_active_origin.set(Some(if start_is_ascii {
                                        SelectionOrigin::Ascii
                                    } else {
                                        SelectionOrigin::Hex
                                    }));

                                    if let Some(set_g) = set_global {
                                        set_g.set(Some(SelectionRange::new(
                                            min,
                                            max_exclusive,
                                            0,
                                            0,
                                            SelectionSource::HexView,
                                        )));
                                    }
                                }
                                return;
                            }

                            let (min, max) = if start_off < end_off {
                                (start_off, end_off)
                            } else {
                                (end_off, start_off)
                            };

                            // Use Range API for precise end boundary:
                            // When endOffset == 0, the browser selection ends just before
                            // the focus node's text content (byte not visually selected).
                            let range_end_offset = selection
                                .get_range_at(0)
                                .ok()
                                .and_then(|r| r.end_offset().ok())
                                .unwrap_or(1);

                            let max_exclusive = if range_end_offset == 0 && min < max {
                                max
                            } else {
                                max + 1
                            };

                            if min >= max_exclusive {
                                return;
                            }

                            // Valid HexView selection found
                            set_active_origin.set(Some(if start_is_ascii {
                                SelectionOrigin::Ascii
                            } else {
                                SelectionOrigin::Hex
                            }));

                            if let Some(set_g) = set_global {
                                set_g.set(Some(SelectionRange::new(
                                    min,
                                    max_exclusive,
                                    0,
                                    0,
                                    SelectionSource::HexView,
                                )));
                            }
                        }
                    }
                }
            }
        }) as Box<dyn FnMut()>);

        let Some(window) = web_sys::window() else {
            return;
        };
        let Some(document) = window.document() else {
            return;
        };
        let _ = document
            .add_event_listener_with_callback("selectionchange", callback.as_ref().unchecked_ref());

        on_cleanup(move || {
            let _ = document.remove_event_listener_with_callback(
                "selectionchange",
                callback.as_ref().unchecked_ref(),
            );
            // Note: Callback remains in memory after removal.
            // wasm-bindgen Closure::forget() prevents the destructor from
            // invalidating the JS function wrapper, which is required for
            // correct cleanup in WASM event handler lifecycle.
            callback.forget();
        });
    });
}
