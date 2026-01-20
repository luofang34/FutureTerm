use core_types::{RawEvent, SelectionRange, SelectionSource};
use leptos::*;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;

/// Represents a single hex dump row (16 or 32 bytes)
#[derive(Clone, Debug, PartialEq)]
struct HexRow {
    offset: usize,
    bytes: Vec<u8>,
}

impl HexRow {
    fn ascii(&self) -> String {
        self.bytes
            .iter()
            .map(|&b| {
                if (32..=126).contains(&b) {
                    b as char
                } else {
                    '.'
                }
            })
            .collect()
    }

    /// Returns groups of up to 4 bytes each
    fn byte_groups(&self) -> Vec<Vec<u8>> {
        self.bytes.chunks(4).map(|chunk| chunk.to_vec()).collect()
    }
}

#[component]
pub fn HexView(
    raw_log: ReadSignal<Vec<RawEvent>>,
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

    // Local selection state (during drag operation)
    let (local_selection_start, set_local_selection_start) = create_signal::<Option<usize>>(None);
    let (is_selecting, set_is_selecting) = create_signal(false);

    // Row Height Constant (Estimate based on CSS)
    const ROW_HEIGHT: f64 = 28.0;

    // Setup ResizeObserver for container
    create_effect(move |_| {
        if let Some(container) = container_ref.get() {
            let set_bpr = set_bytes_per_row.clone();
            let set_h = set_container_height.clone();

            // Initial check
            let initial_width = container.client_width() as f64;
            let initial_height = container.client_height() as f64;
            set_h.set(initial_height);

            // 32 bytes needs ~1150px. 16 bytes needs ~700px.
            if initial_width >= 1150.0 {
                set_bytes_per_row.set(32);
            } else {
                set_bytes_per_row.set(16);
            }

            let callback = Closure::wrap(Box::new(move |entries: js_sys::Array| {
                for entry in entries.iter() {
                    if let Ok(entry) = entry.dyn_into::<web_sys::ResizeObserverEntry>() {
                        // Use contentRect for precise content box measurement
                        let rect = entry.content_rect();
                        let width = rect.width();
                        let height = rect.height();

                        set_h.set(height);

                        // Hysteresis to prevent flickering
                        if width >= 1150.0 {
                            set_bpr.set(32);
                        } else if width < 1120.0 {
                            set_bpr.set(16);
                        }
                    }
                }
            }) as Box<dyn FnMut(js_sys::Array)>);

            if let Ok(observer) = web_sys::ResizeObserver::new(callback.as_ref().unchecked_ref()) {
                observer.observe(&container);
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
        let mut current_offset = 0;
        let bpr = bytes_per_row.get();

        // Process all events from raw log (cursor used for tail-follow, not filtering)
        for raw_event in raw_log.get() {
            let bytes = &raw_event.bytes;
            for chunk in bytes.chunks(bpr) {
                rows.push(HexRow {
                    offset: current_offset,
                    bytes: chunk.to_vec(),
                });
                current_offset += chunk.len();
            }
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
        let buffer = 5;
        let start_idx = if start_idx > buffer {
            start_idx - buffer
        } else {
            0
        };

        let visible_count = (viewport_h / ROW_HEIGHT).ceil() as usize + (buffer * 2);
        let end_idx = (start_idx + visible_count).min(total_count);

        let slice = rows[start_idx..end_idx].to_vec();

        let padding_top = start_idx as f64 * ROW_HEIGHT;
        let padding_bottom = (total_count - end_idx) as f64 * ROW_HEIGHT;

        (padding_top, padding_bottom, slice)
    });

    // Auto-scroll logic: Only snap if we are already near bottom or explicitly requested?
    // User requested simpler logic before, but virtualization complicates "scroll to bottom".
    // If the valid data grows, we might want to auto-scroll.
    // However, tracking scroll status is cleaner. For now, let's keep it simple:
    // If we receive new events, we update the list. Sticky scroll is hard with virtualization without tracking "is_at_bottom".
    // Let's assume the user wants to see the latest data if they haven't scrolled up.

    // Actually, simply setting scroll_top to scroll_height on new data is a valid strategy for "terminal like" behavior
    // BUT we need to check if user scrolled up manually.
    // For this refactor, let's focus on the rendering optimization requested.
    // The previous implementation used `div.set_scroll_top(div.scroll_height())`.
    create_effect(move |_| {
        // Trigger on dependency
        all_hex_rows.with(|_| {});

        // Naive auto-scroll (can be improved later)
        if let Some(div) = container_ref.get() {
            // Only auto-scroll if we were recently at the bottom?
            // Or always? The user didn't specify, but regular terminal rules apply.
            // Let's scroll to bottom if we are adding data.
            // We can check `div.scroll_top() + div.client_height() >= div.scroll_height() - 10.0`
            div.set_scroll_top(div.scroll_height());
        }
    });

    // Auto-scroll to selection when it comes from another view
    create_effect(move |_| {
        if let Some(global_sel) = global_selection {
            if let Some(range) = global_sel.get() {
                if range.source_view != SelectionSource::HexView {
                    // Selection came from another view, scroll to it
                    let bpr = bytes_per_row.get();
                    let target_row = range.start_byte_offset / bpr;
                    let target_scroll = (target_row as f64) * ROW_HEIGHT;

                    if let Some(div) = container_ref.get() {
                        div.set_scroll_top(target_scroll as i32);
                    }
                }
            }
        }
    });

    // Grid Template: Offset | Hex Data | Separator | ASCII
    // Use max-content for ASCII column to prevent layout issues with partial rows
    let grid_template = create_memo(move |_| {
        "8ch max-content 1px max-content".to_string()
    });

    view! {
        <div
            _ref=container_ref
            class="hex-view"
            on:scroll=move |ev| {
                let div = event_target::<web_sys::HtmlElement>(&ev);
                set_scroll_top.set(div.scroll_top() as f64);
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
                    min-width: 100%;",
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
                                        <span style="flex: 1; text-align: center;">{format!("{:02X}", i)}</span>
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
                        let ascii_text = row.ascii();
                        let offset = row.offset;
                        let bpr = bytes_per_row.get();

                        view! {
                            <div
                                class="hex-row"
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
                            >
                                // Offset
                                <div style="color: #858585; font-weight: bold;">
                                    {format!("{:08X}", offset)}
                                </div>

                                // Hex Groups (Padded)
                                <div style="display: flex; gap: 16px;">
                                    {
                                        let total_groups = bpr / 4;
                                        let current_groups = groups.len();

                                        // 1. Render actual data groups
                                        let mut views = groups.into_iter().enumerate().map(|(group_idx, group)| {
                                            let is_sep = group_idx < total_groups - 1;

                                            // Render each byte with selection support
                                            let byte_views: Vec<_> = (0..4).map(|byte_idx| {
                                                let byte_offset = offset + (group_idx * 4) + byte_idx;

                                                // Get the hex string for this byte (or padding)
                                                let hex_str = group.get(byte_idx)
                                                    .map(|b| format!("{:02X}", b))
                                                    .unwrap_or_else(|| "  ".to_string());

                                                // Check if this byte is selected
                                                let is_selected = move || {
                                                    if let Some(global_sel) = global_selection {
                                                        if let Some(range) = global_sel.get() {
                                                            return range.contains_offset(byte_offset);
                                                        }
                                                    }

                                                    // Check local selection during drag
                                                    if let Some(start) = local_selection_start.get() {
                                                        if is_selecting.get() {
                                                            // Get current mouse position from a potential end point
                                                            // For now, we'll just highlight from start onwards during drag
                                                            return byte_offset >= start;
                                                        }
                                                    }

                                                    false
                                                };

                                                view! {
                                                    <span
                                                        style=move || format!(
                                                            "flex: 1; text-align: center; cursor: pointer; user-select: none; {}",
                                                            if is_selected() {
                                                                "background-color: rgba(86, 156, 214, 0.3); border-radius: 2px;"
                                                            } else {
                                                                ""
                                                            }
                                                        )
                                                        on:mousedown=move |ev| {
                                                            ev.prevent_default();
                                                            set_local_selection_start.set(Some(byte_offset));
                                                            set_is_selecting.set(true);
                                                        }
                                                        on:mouseenter=move |_| {
                                                            if is_selecting.get() && local_selection_start.get().is_some() {
                                                                // Update selection range during drag
                                                                // The highlighting will update reactively
                                                            }
                                                        }
                                                        on:mouseup=move |_| {
                                                            if let Some(start) = local_selection_start.get() {
                                                                set_is_selecting.set(false);

                                                                // Commit selection to global state
                                                                if let Some(set_global_sel) = set_global_selection {
                                                                    let range = SelectionRange::new(
                                                                        start.min(byte_offset),
                                                                        start.max(byte_offset) + 1,
                                                                        0, // TODO: lookup timestamp from raw_log
                                                                        0,
                                                                        SelectionSource::HexView,
                                                                    );
                                                                    set_global_sel.set(Some(range));
                                                                }
                                                            }
                                                        }
                                                    >
                                                        {hex_str}
                                                    </span>
                                                }
                                            }).collect();

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
                                                    <div style=format!("visibility: hidden; display: inline-flex; gap: 6px; min-width: 94px; {}",
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
                                <div style="background: rgba(255, 255, 255, 0.2); width: 1px;"></div>

                                // ASCII
                                <div style="color: #b5cea8; white-space: pre; overflow: hidden; letter-spacing: 0;">
                                    {ascii_text}
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

pub fn icon() -> impl IntoView {
    view! {
        <svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M12 2l9 5v10l-9 5l-9-5V7z" />
            <text x="50%" y="54%" text-anchor="middle" dominant-baseline="middle" font-size="9" font-weight="bold" fill="currentColor" stroke="none">"0x"</text>
        </svg>
    }
}
