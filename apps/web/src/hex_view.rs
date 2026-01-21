use core_types::{RawEvent, SelectionRange, SelectionSource};
use leptos::*;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;

// Display constants
/// Height of each hex row in pixels
const ROW_HEIGHT: f64 = 28.0;

/// Auto-scroll threshold distance from bottom in pixels
const AUTO_SCROLL_THRESHOLD: f64 = 100.0;

/// Number of buffer rows to prevent white flashes during scroll
const SCROLL_BUFFER_ROWS: usize = 5;

// Responsive layout breakpoints
/// Minimum width in pixels for 32-byte row layout
const WIDE_LAYOUT_MIN_WIDTH: f64 = 1150.0;

/// Width hysteresis threshold to prevent flickering
const WIDE_LAYOUT_HYSTERESIS: f64 = 1120.0;

/// Represents a single hex dump row (16 or 32 bytes)
#[derive(Clone, Debug, PartialEq)]
struct HexRow {
    offset: usize,
    bytes: Vec<u8>,
}

impl HexRow {
    #[allow(dead_code)]
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

    #[derive(Clone, Copy, PartialEq, Debug)]
    enum SelectionOrigin {
        Hex,
        Ascii,
    }
    let (active_origin, set_active_origin) = create_signal::<Option<SelectionOrigin>>(None);
    // Lock signal for overflow protection: When one side is active, lock the other side.
    let (selection_lock, set_selection_lock) = create_signal::<Option<SelectionOrigin>>(None);

    // Clear lock on global mouseup and mouseleave (handles edge case of mouse leaving window)
    create_effect(move |_| {
        let window = web_sys::window().unwrap();

        let mouseup_callback = Closure::wrap(Box::new(move || {
            set_selection_lock.set(None);
        }) as Box<dyn FnMut()>);

        let mouseleave_callback = Closure::wrap(Box::new(move || {
            set_selection_lock.set(None);
        }) as Box<dyn FnMut()>);

        let _ = window
            .add_event_listener_with_callback("mouseup", mouseup_callback.as_ref().unchecked_ref());
        let _ = window.add_event_listener_with_callback(
            "mouseleave",
            mouseleave_callback.as_ref().unchecked_ref(),
        );

        on_cleanup(move || {
            if let Some(w) = web_sys::window() {
                let _ = w.remove_event_listener_with_callback(
                    "mouseup",
                    mouseup_callback.as_ref().unchecked_ref(),
                );
                let _ = w.remove_event_listener_with_callback(
                    "mouseleave",
                    mouseleave_callback.as_ref().unchecked_ref(),
                );
            }
            mouseup_callback.forget();
            mouseleave_callback.forget();
        });
    });

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

            // 32 bytes needs ~1150px. 16 bytes needs ~700px.
            if initial_width >= WIDE_LAYOUT_MIN_WIDTH {
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

                        // Use try_set to avoid warnings when signals are disposed
                        let _ = set_h.try_set(height);

                        // Hysteresis to prevent flickering
                        if width >= WIDE_LAYOUT_MIN_WIDTH {
                            let _ = set_bpr.try_set(32);
                        } else if width < WIDE_LAYOUT_HYSTERESIS {
                            let _ = set_bpr.try_set(16);
                        }
                    }
                }
            }) as Box<dyn FnMut(js_sys::Array)>);

            if let Ok(observer) = web_sys::ResizeObserver::new(callback.as_ref().unchecked_ref()) {
                observer.observe(&container);

                // Store observer and callback for cleanup
                let observer_clone = observer.clone();
                on_cleanup(move || {
                    observer_clone.disconnect();
                });

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
        let start_idx = start_idx.saturating_sub(SCROLL_BUFFER_ROWS);

        let visible_count = (viewport_h / ROW_HEIGHT).ceil() as usize + (SCROLL_BUFFER_ROWS * 2);
        let end_idx = (start_idx + visible_count).min(total_count);

        let slice = rows[start_idx..end_idx].to_vec();

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

                    web_sys::console::log_1(&format!(
                        "HexView received selection from {:?}: bytes {}-{} (count: {}), scrolling to row {} ({}px), should highlight ~{} rows",
                        range.source_view, range.start_byte_offset, range.end_byte_offset,
                        byte_count, target_row, target_scroll, expected_rows
                    ).into());

                    if let Some(div) = container_ref.get() {
                        div.set_scroll_top(target_scroll as i32);
                    }
                }
            }
        }
    });

    // Handle Native Selection Changes (Throttled for performance)
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
                            // Cache element lookups for performance
                            let mut offset_found = None;
                            let mut is_ascii_found = None;

                            while let Some(n) = curr {
                                if let Some(el) = n.dyn_ref::<web_sys::HtmlElement>() {
                                    if offset_found.is_none() {
                                        if let Some(bg) = el.dataset().get("offset") {
                                            if let Ok(offset) = bg.parse::<usize>() {
                                                offset_found = Some(offset);
                                            }
                                        }
                                    }
                                    if is_ascii_found.is_none()
                                        && el.class_list().contains("ascii-char")
                                    {
                                        is_ascii_found = Some(true);
                                    }
                                    if offset_found.is_some() && is_ascii_found.is_some() {
                                        break;
                                    }
                                }
                                curr = n.parent_element().map(|e| e.into());
                            }
                            offset_found.map(|off| (off, is_ascii_found.unwrap_or(false)))
                        };

                        let start_info = get_info(anchor);
                        let end_info = get_info(focus);

                        if let (Some((start_off, start_is_ascii)), Some((end_off, _))) =
                            (start_info, end_info)
                        {
                            let (min, max) = if start_off < end_off {
                                (start_off, end_off)
                            } else {
                                (end_off, start_off)
                            };

                            // Valid HexView selection found
                            set_active_origin.set(Some(if start_is_ascii {
                                SelectionOrigin::Ascii
                            } else {
                                SelectionOrigin::Hex
                            }));

                            if let Some(set_g) = set_global {
                                set_g.set(Some(SelectionRange::new(
                                    min,
                                    max + 1,
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

        let document = web_sys::window().unwrap().document().unwrap();
        let _ = document
            .add_event_listener_with_callback("selectionchange", callback.as_ref().unchecked_ref());

        on_cleanup(move || {
            let _ = document.remove_event_listener_with_callback(
                "selectionchange",
                callback.as_ref().unchecked_ref(),
            );
            callback.forget();
        });
    });

    // Direct DOM manipulation for highlighting (maximum performance)
    // This bypasses Leptos reactivity and updates classes directly
    create_effect(move |_| {
        let range_opt = global_selection.and_then(|g| g.get());
        let origin = active_origin.get();

        // Use requestAnimationFrame to batch DOM updates
        let callback = Closure::once(Box::new(move || {
            if let Some(window) = web_sys::window() {
                if let Some(document) = window.document() {
                    // Clear all previous highlights efficiently
                    if let Ok(elements) = document.query_selector_all(".hex-byte") {
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
                                                    // Terminal selection: both hex and ASCII get bg-term
                                                    if is_terminal {
                                                        let _ = el.class_list().add_1("bg-term");
                                                    }
                                                    // HexView selection: sync highlighting logic
                                                    else if is_hex_view {
                                                        let is_ascii =
                                                            el.class_list().contains("ascii-char");
                                                        // If origin is ASCII and this is hex, or vice versa, apply sync color
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
                }
            }
        }) as Box<dyn FnOnce()>);

        if let Some(window) = web_sys::window() {
            let _ = window.request_animation_frame(callback.as_ref().unchecked_ref());
            callback.forget();
        }
    });

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
            on:copy=move |ev: web_sys::Event| {
                let Some(ev) = ev.dyn_into::<web_sys::ClipboardEvent>().ok() else { return; };
                if let Some(window) = web_sys::window() {
                    if let Some(selection) = window.get_selection().ok().flatten() {
                        if selection.is_collapsed() { return; }

                        let anchor_node = selection.anchor_node();
                        let focus_node = selection.focus_node();

                        if let (Some(anchor), Some(focus)) = (anchor_node, focus_node) {
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
                                    _ => None
                                }
                            };

                            let start_info = get_info(anchor);
                            let end_info = get_info(focus);

                            if let (Some((start_off, start_ascii)), Some((end_off, _))) = (start_info, end_info) {
                                let (min, max) = if start_off < end_off { (start_off, end_off) } else { (end_off, start_off) };

                                let mut content = String::new();
                                let rows = all_hex_rows.get();

                                for row in rows {
                                    if row.offset + row.bytes.len() <= min { continue; }
                                    if row.offset > max { break; }

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
                                                if !content.is_empty() && content.len() % 3 == 2 { content.push(' '); }
                                                else if !content.is_empty() && content.ends_with('\n') { /* Newline, no space needed */ }
                                                else if !content.is_empty() { content.push(' '); }

                                                content.push_str(&format!("{:02X}", b));
                                            }
                                        }
                                    }
                                    // Add newlines? Browser copy usually adds newlines for block elements.
                                    // But here we are constructing a string.
                                    // If the selection spans multiple rows, we might want newlines?
                                    // The user "multi line selection" implies block copy.
                                    // Logic: If we finished a row and there are more bytes in range, add newline?
                                    // Simpler: Just space separated hex is usually preferred, but for long dumps, lines are good.
                                    // Let's stick to space separated for now to be safe, or check existing behavior.
                                    // Existing behavior: one long string.
                                }

                                if let Some(clipboard_data) = ev.clipboard_data() {
                                    let _ = clipboard_data.set_data("text/plain", &content);
                                    ev.prevent_default();
                                }
                            }
                        }
                    }
                }
            }
            // Note: Browser's default copy behavior will include both hex and ASCII columns
            // if user selects across the grid. This is a known limitation of the grid layout.
            // Users should select within a single column for best results.
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
            <style>
                ".hex-byte {
                     cursor: text;
                     display: inline-block;
                     padding: 0 1px;
                     box-sizing: border-box;
                 }
                 .ascii-char {
                     cursor: text;
                     display: inline-block;
                     padding: 0;
                 }
                 .bg-sync {
                     background-color: rgba(80, 150, 250, 0.4);
                 }
                 .bg-term {
                     background-color: rgba(86, 156, 214, 0.3);
                 }
                 .hex-byte::selection, .ascii-char::selection {
                     background-color: rgba(80, 150, 250, 0.4);
                 }
                 .selection-locked {
                     user-select: none !important;
                 }
                 .selection-locked * {
                     user-select: none !important;
                 }"
            </style>

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
                                    class:selection-locked=move || selection_lock.get() == Some(SelectionOrigin::Ascii)
                                    on:mousedown=move |_| set_selection_lock.set(Some(SelectionOrigin::Hex))
                                    style="display: flex; gap: 16px; user-select: text;"
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
                                                let byte_offset = offset + (group_idx * 4) + byte_idx;
                                                let hex_str = bytes_for_group.get(byte_idx)
                                                    .map(|b| format!("{:02X}", b))
                                                    .unwrap_or_else(|| "  ".to_string());

                                                view! {
                                                    <span
                                                        class="hex-byte"
                                                        data-offset={byte_offset.to_string()}
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
                                <div style="background: rgba(255, 255, 255, 0.2); width: 1px; height: 100%;"></div>

                                // ASCII
                                <div
                                    class="ascii-container"
                                    class:selection-locked=move || selection_lock.get() == Some(SelectionOrigin::Hex)
                                    on:mousedown=move |_| set_selection_lock.set(Some(SelectionOrigin::Ascii)) // Lock Hex when clicking ASCII
                                    style="color: #b5cea8; white-space: pre; overflow: hidden; letter-spacing: 0; display: inline-flex; user-select: text;">
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

pub fn icon() -> impl IntoView {
    view! {
        <svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
            <path d="M12 2l9 5v10l-9 5l-9-5V7z" />
            <text x="50%" y="54%" text-anchor="middle" dominant-baseline="middle" font-size="9" font-weight="bold" fill="currentColor" stroke="none">"0x"</text>
        </svg>
    }
}
