use core_types::{RawEvent, SelectionRange, SelectionSource};
use leptos::*;
use std::collections::VecDeque;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;

mod highlight;
mod layout;
mod selection;

pub use layout::icon;
use layout::{
    HexRow, SelectionOrigin, AUTO_SCROLL_THRESHOLD, HEX_STYLES, MEDIUM_LAYOUT_HYSTERESIS,
    MEDIUM_LAYOUT_MIN_WIDTH, NARROW_LAYOUT_HYSTERESIS, NARROW_LAYOUT_MIN_WIDTH, ROW_HEIGHT,
    SCROLL_BUFFER_ROWS, WIDE_LAYOUT_HYSTERESIS, WIDE_LAYOUT_MIN_WIDTH,
};

#[component]
pub fn HexView(
    raw_log: ReadSignal<VecDeque<RawEvent>>,
    cursor: ReadSignal<usize>,
    set_cursor: WriteSignal<usize>,
    #[prop(optional)] global_selection: Option<ReadSignal<Option<SelectionRange>>>,
    #[prop(optional)] set_global_selection: Option<WriteSignal<Option<SelectionRange>>>,
) -> impl IntoView {
    let container_ref = create_node_ref::<html::Div>();

    // Signal State
    let (bytes_per_row, set_bytes_per_row) = create_signal(16usize);
    let (container_height, set_container_height) = create_signal(600.0); // Default height
    let (scroll_top, set_scroll_top) = create_signal(0.0);

    let (active_origin, set_active_origin) = create_signal::<Option<SelectionOrigin>>(None);

    // Row height is defined as a module-level constant

    // Setup ResizeObserver for container
    create_effect(move |_| {
        if let Some(container) = container_ref.get() {
            let set_bpr = set_bytes_per_row;
            let set_h = set_container_height;

            // Initial check
            let initial_width = container.client_width() as f64;
            let initial_height = container.client_height() as f64;
            set_h.set(initial_height);

            if initial_width >= WIDE_LAYOUT_MIN_WIDTH {
                set_bytes_per_row.set(32);
            } else if initial_width >= MEDIUM_LAYOUT_MIN_WIDTH {
                set_bytes_per_row.set(16);
            } else if initial_width >= NARROW_LAYOUT_MIN_WIDTH {
                set_bytes_per_row.set(8);
            } else {
                set_bytes_per_row.set(4);
            }

            let callback = Closure::wrap(Box::new(move |entries: js_sys::Array| {
                for entry in entries.iter() {
                    if let Ok(entry) = entry.dyn_into::<web_sys::ResizeObserverEntry>() {
                        // Use contentRect for precise content box measurement
                        let rect = entry.content_rect();
                        let width = rect.width();
                        let height = rect.height();

                        // Use try_set to avoid warnings when signals are disposed
                        let _ = set_h.try_set(height);

                        // Hysteresis to prevent flickering at breakpoints
                        if width >= WIDE_LAYOUT_MIN_WIDTH {
                            let _ = set_bpr.try_set(32);
                        } else if (MEDIUM_LAYOUT_MIN_WIDTH..WIDE_LAYOUT_HYSTERESIS).contains(&width)
                        {
                            let _ = set_bpr.try_set(16);
                        } else if (NARROW_LAYOUT_MIN_WIDTH..MEDIUM_LAYOUT_HYSTERESIS)
                            .contains(&width)
                        {
                            let _ = set_bpr.try_set(8);
                        } else if width < NARROW_LAYOUT_HYSTERESIS {
                            let _ = set_bpr.try_set(4);
                        }
                    }
                }
            }) as Box<dyn FnMut(js_sys::Array)>);

            if let Ok(observer) = web_sys::ResizeObserver::new(callback.as_ref().unchecked_ref()) {
                observer.observe(&container);

                // Store observer for cleanup
                // Note: callback must remain alive for the observer, so we intentionally
                // keep it in memory. This is unavoidable with ResizeObserver API.
                let observer_clone = observer.clone();
                on_cleanup(move || {
                    observer_clone.disconnect();
                });

                // Intentionally keep callback alive for observer lifetime
                callback.forget();
            }
        }
    });

    // Auto-advance cursor in tail-follow mode
    // This effect runs when raw_log grows, and advances cursor if we're at the end
    create_effect(move |prev_len: Option<usize>| {
        let log = raw_log.get();
        let current_len = log.len();

        // Only auto-advance if we were at the end (tail-follow mode)
        if let Some(prev) = prev_len {
            if cursor.get_untracked() == prev {
                set_cursor.set(current_len);
            }
        } else {
            // First run, set cursor to end
            set_cursor.set(current_len);
        }

        current_len
    });

    // Process raw events into rows based on current bytes_per_row
    let all_hex_rows = create_memo(move |_| {
        let mut rows = Vec::new();
        let bpr = bytes_per_row.get();

        // Accumulate all bytes across events, then chunk uniformly
        let all_bytes: Vec<u8> = raw_log
            .get()
            .iter()
            .flat_map(|ev| ev.bytes.iter().copied())
            .collect();

        for (i, chunk) in all_bytes.chunks(bpr).enumerate() {
            rows.push(HexRow {
                offset: i * bpr,
                bytes: chunk.to_vec(),
            });
        }
        rows
    });

    // Virtual Scroll Logic
    let visible_rows = create_memo(move |_| {
        let rows = all_hex_rows.get();
        let total_count = rows.len();
        if total_count == 0 {
            return (0.0, 0.0, Vec::new());
        }

        let viewport_h = container_height.get();
        let scroll_y = scroll_top.get();

        let start_idx = (scroll_y / ROW_HEIGHT).floor() as usize;
        // Buffer rows to prevent white flashes
        let start_idx = start_idx.saturating_sub(SCROLL_BUFFER_ROWS);

        let visible_count = (viewport_h / ROW_HEIGHT).ceil() as usize + (SCROLL_BUFFER_ROWS * 2);
        let end_idx = (start_idx + visible_count).min(total_count);

        let slice = rows
            .get(start_idx..end_idx)
            .map(|s| s.to_vec())
            .unwrap_or_default();

        let padding_top = start_idx as f64 * ROW_HEIGHT;
        let padding_bottom = (total_count - end_idx) as f64 * ROW_HEIGHT;

        (padding_top, padding_bottom, slice)
    });

    // Auto-scroll: Only scroll to bottom if user is already near bottom (tail-follow mode)
    // This prevents auto-scroll from disrupting manual scrolling
    create_effect(move |_| {
        // Trigger on new data
        all_hex_rows.with(|_| {});

        if let Some(div) = container_ref.get() {
            let scroll_top = div.scroll_top() as f64;
            let client_height = div.client_height() as f64;
            let scroll_height = div.scroll_height() as f64;

            // Only auto-scroll if user is near bottom (tail-follow mode)
            let is_near_bottom =
                scroll_top + client_height >= scroll_height - AUTO_SCROLL_THRESHOLD;

            if is_near_bottom {
                div.set_scroll_top(div.scroll_height());
            }
        }
    });

    // TODO: Implement copy handler with clipboard API
    // For now, copy behavior will use browser's default text selection
    // This requires making hex bytes and ASCII text selectable

    // Auto-scroll to selection when it comes from another view
    create_effect(move |_| {
        if let Some(global_sel) = global_selection {
            if let Some(range) = global_sel.get() {
                if range.source_view != SelectionSource::HexView {
                    // Selection came from another view, scroll to it
                    let bpr = bytes_per_row.get();
                    let target_row = range.start_byte_offset / bpr;
                    let target_scroll = (target_row as f64) * ROW_HEIGHT;
                    let byte_count = range.end_byte_offset - range.start_byte_offset;
                    let expected_rows = byte_count.div_ceil(bpr); // Ceiling division

                    #[cfg(debug_assertions)]
                    web_sys::console::log_1(
                        &format!(
                            "HexView received selection from {:?}: bytes {}-{} (count: {}), \
                             scrolling to row {} ({}px), should highlight ~{} rows",
                            range.source_view,
                            range.start_byte_offset,
                            range.end_byte_offset,
                            byte_count,
                            target_row,
                            target_scroll,
                            expected_rows
                        )
                        .into(),
                    );

                    if let Some(div) = container_ref.get() {
                        div.set_scroll_top(target_scroll as i32);
                    }
                }
            }
        }
    });

    // Native selection listener (extracted to keep this file reviewable).
    selection::setup_selection_change_listener(set_active_origin, set_global_selection);

    // Highlight-class DOM updates (extracted to keep this file reviewable).
    highlight::setup_highlight_effect(global_selection, active_origin);

    // Grid Template: Offset | Hex Data | Separator | ASCII
    // Calculate fixed width for hex column to prevent ASCII invasion
    let grid_template = create_memo(move |_| {
        let bpr = bytes_per_row.get();
        // Each group: 4 bytes * ~24px + gaps + separators ≈ 94px + 8px padding + 16px gap
        // For 16 bytes: 4 groups * (94px + 24px gap) ≈ 472px
        // For 32 bytes: 8 groups * (94px + 24px gap) ≈ 944px
        let num_groups = bpr / 4;
        let hex_width = num_groups * 94 + (num_groups - 1) * 24; // groups + gaps between them
        format!("8ch {}px 1px max-content", hex_width)
    });

    view! {
        <div
            _ref=container_ref
            class="hex-view"
            on:scroll=move |ev| {
                let div = event_target::<web_sys::HtmlElement>(&ev);
                set_scroll_top.set(div.scroll_top() as f64);
            }
            on:copy=selection::make_copy_handler(all_hex_rows)
            on:mouseup=move |_ev| {
                // DISABLED: Custom selection causing performance issues and conflicts with browser copy
                // Users can use browser's native text selection to copy hex bytes or ASCII
                // Custom highlighting only used for Terminal→Hex sync
            }
            on:mouseleave=move |_| {
                // DISABLED: Custom selection removed
            }
            style="
                width: 100%;
                height: 100%;
                background: rgb(25, 25, 25);
                color: #d4d4d4;
                font-family: 'Menlo', 'Monaco', 'Consolas', 'Courier New', monospace;
                font-size: 13px;
                overflow-y: auto;
                overflow-x: auto;
                box-sizing: border-box;
                position: relative;
            "
        >
            // Performance Styles
            <style>{HEX_STYLES}</style>

            // Sticky Header
            <div
                class="hex-header"
                style=move || format!(
                    "position: sticky; \
                    top: 0; \
                    z-index: 10; \
                    background: rgb(25, 25, 25); \
                    display: grid; \
                    grid-template-columns: {}; \
                    gap: 12px; \
                    padding: 8px 12px; \
                    border-bottom: 2px solid #569cd6; \
                    font-weight: bold; \
                    color: #569cd6; \
                    width: max-content; \
                    min-width: 100%; \
                    user-select: none;",
                    grid_template.get()
                )
            >
                <div>OFFSET</div>
                <div style="display: flex; gap: 16px;">
                    {move || {
                        let bpr = bytes_per_row.get();
                        let num_groups = bpr / 4;
                        (0..num_groups).map(|group_idx| {
                            let start = group_idx * 4;
                            view! {
                                <div
                                    style=format!(
                                        "display: inline-flex; gap: 6px; min-width: 94px; justify-content: start; {}",
                                        if group_idx < num_groups - 1 {
                                            "padding-right: 8px; border-right: 1px solid rgba(255, 255, 255, 0.1);"
                                        } else {
                                            ""
                                        }
                                    )
                                >
                                    {(start..start+4).map(|i| view! {
                                        <span style="width: 2ch; text-align: center; display: inline-block;">{format!("{:02X}", i)}</span>
                                    }).collect::<Vec<_>>()}
                                </div>
                            }
                        }).collect::<Vec<_>>()
                    }}
                </div>
                // Separator column
                <div style="background: rgba(255, 255, 255, 0.2); width: 1px; height: 100%;"></div>
                <div>ASCII</div>
            </div>

            // Data Content
            <div style="width: max-content; min-width: 100%;">
                // Top Padding (Virtual Scroll)
                <div style=move || format!("height: {}px;", visible_rows.get().0)></div>

                <For
                    each=move || visible_rows.get().2
                    key=|row| (row.offset, row.bytes.len())
                    children=move |row: HexRow| {
                        let groups = row.byte_groups();
                        let offset = row.offset;
                        let bpr = bytes_per_row.get();

                        view! {
                            <div
                                class="hex-row"
                                on:mousedown=move |_ev| {
                                    // DISABLED: Custom selection removed, use browser's native text selection
                                }
                                on:mousemove=move |_ev| {
                                    // DISABLED: Custom selection removed
                                }
                                style=move || format!(
                                    "display: grid; \
                                    grid-template-columns: {}; \
                                    gap: 12px; \
                                    padding: 4px 12px; \
                                    height: {}px; \
                                    box-sizing: border-box; \
                                    border-bottom: 1px solid #2d2d2d;",
                                    grid_template.get(),
                                    ROW_HEIGHT
                                )
                                data-row-offset={offset.to_string()}
                            >
                                // Offset
                                <div style="color: #858585; font-weight: bold; user-select: none;">
                                    {format!("{:08X}", offset)}
                                </div>

                                // Hex Groups (Padded)
                                <div
                                    class="hex-data-container"
                                    style=move || format!(
                                        "display: flex; gap: 16px; user-select: {};",
                                        if active_origin.get() == Some(SelectionOrigin::Ascii) { "none" } else { "text" }
                                    )
                                >
                                    {
                                        let total_groups = bpr / 4;
                                        let current_groups = groups.len();

                                        // 1. Render actual data groups
                                        let mut views = groups.into_iter().enumerate().map(|(group_idx, group)| {
                                            let is_sep = group_idx < total_groups - 1;

                                            // Render each byte with selection support
                                            let bytes_for_group = group.clone();
                                            let _group_len = bytes_for_group.len();
                                            let byte_views = (0..4).map(|byte_idx| {
                                                let has_data = bytes_for_group.get(byte_idx).is_some();
                                                let byte_offset = offset + (group_idx * 4) + byte_idx;
                                                let hex_str = bytes_for_group.get(byte_idx)
                                                    .map(|b| format!("{:02X}", b))
                                                    .unwrap_or_else(|| "  ".into());

                                                // Only set data-offset for real bytes, not padding
                                                let offset_attr = if has_data {
                                                    Some(byte_offset.to_string())
                                                } else {
                                                    None
                                                };

                                                view! {
                                                    <span
                                                        class="hex-byte"
                                                        class:hex-pad=move || !has_data
                                                        data-offset=offset_attr
                                                        style=if has_data { "" } else { "user-select: none;" }
                                                    >
                                                        {hex_str}
                                                    </span>
                                                }
                                            }).collect::<Vec<_>>();

                                            view! {
                                                <div style=format!("color: #ce9178; display: inline-flex; gap: 6px; min-width: 94px; justify-content: start; {}",
                                                    if is_sep { "padding-right: 8px; border-right: 1px solid rgba(255, 255, 255, 0.1);" } else { "" }
                                                )>
                                                    {byte_views}
                                                </div>
                                            }
                                        }).collect::<Vec<_>>();

                                        // 2. Render placeholders for missing groups
                                        if current_groups < total_groups {
                                            for idx in current_groups..total_groups {
                                                 let is_sep = idx < total_groups - 1;
                                                 views.push(view! {
                                                    <div style=format!("color: transparent; user-select: none; display: inline-flex; gap: 6px; min-width: 94px; {}",
                                                        if is_sep { "padding-right: 8px; border-right: 1px solid rgba(255, 255, 255, 0.1);" } else { "" }
                                                    )>
                                                        // 4 placeholders to maintain width
                                                        <span>"00"</span><span>"00"</span><span>"00"</span><span>"00"</span>
                                                    </div>
                                                });
                                            }
                                        }
                                        views
                                    }
                                </div>

                                // Separator
                                <div style="background: rgba(255, 255, 255, 0.2); width: 1px; height: 100%; user-select: none;"></div>

                                // ASCII
                                <div
                                    class="ascii-container"
                                    style=move || format!(
                                        "color: #b5cea8; white-space: pre; overflow: hidden; letter-spacing: 0; display: inline-flex; user-select: {};",
                                        if active_origin.get() == Some(SelectionOrigin::Hex) { "none" } else { "text" }
                                    )>
                                    {
                                        // Render each ASCII character separately for selection
                                        row.bytes.iter().enumerate().map(|(idx, &b)| {
                                            let byte_offset = offset + idx;
                                            let ascii_char = if (32..=126).contains(&b) {
                                                (b as char).to_string()
                                            } else {
                                                ".".to_string()
                                            };

                                            view! {
                                                <span
                                                    class="hex-byte ascii-char"
                                                    data-offset={byte_offset.to_string()}
                                                >
                                                    {ascii_char}
                                                </span>
                                            }
                                        }).collect::<Vec<_>>()
                                    }
                                </div>
                            </div>
                        }
                    }
                />

                // Bottom Padding
                <div style=move || format!("height: {}px;", visible_rows.get().1)></div>
            </div>
        </div>
    }
}
