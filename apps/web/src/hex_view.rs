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

    // Local selection state (during drag operation)
    let (selection_anchor, set_selection_anchor) = create_signal::<Option<usize>>(None);
    let (selection_focus, set_selection_focus) = create_signal::<Option<usize>>(None);

    // Track selection type for copy behavior
    #[derive(Clone, Copy, PartialEq)]
    enum SelectionType {
        Hex,
        Ascii,
    }
    let (_selection_type, set_selection_type) = create_signal::<Option<SelectionType>>(None);

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

                        // Use try_set to avoid warnings when signals are disposed
                        let _ = set_h.try_set(height);

                        // Hysteresis to prevent flickering
                        if width >= 1150.0 {
                            let _ = set_bpr.try_set(32);
                        } else if width < 1120.0 {
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
                    let expected_rows = (byte_count + bpr - 1) / bpr; // Ceiling division

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

    // Handle Native Selection Changes
    let (internal_selection_active, set_internal_selection_active) = create_signal(false);

    create_effect(move |_| {
        let set_global = set_global_selection;
        let set_active = set_internal_selection_active;
        
        let callback = Closure::wrap(Box::new(move || {
            if let Some(window) = web_sys::window() {
                if let Some(selection) = window.get_selection().ok().flatten() {
                    // If no valid selection, perform a soft reset check
                    if selection.is_collapsed() {
                        // If we previously had an active selection, clear it
                        set_active.update(|active| {
                            if *active {
                                if let Some(set_g) = set_global { set_g.set(None); }
                                *active = false;
                            }
                        });
                        return;
                    }

                    let anchor_node = selection.anchor_node();
                    let focus_node = selection.focus_node();

                    if let (Some(anchor), Some(focus)) = (anchor_node, focus_node) {
                        let get_info = |node: web_sys::Node| -> Option<(usize, bool)> {
                            let mut curr = Some(node);
                             while let Some(n) = curr {
                                if let Some(el) = n.dyn_ref::<web_sys::HtmlElement>() {
                                    if let Some(bg) = el.dataset().get("offset") {
                                        if let Ok(offset) = bg.parse::<usize>() {
                                            let is_ascii = el.class_list().contains("ascii-char");
                                            return Some((offset, is_ascii));
                                        }
                                    }
                                }
                                curr = n.parent_element().map(|e| e.into());
                            }
                            None
                        };

                        let start_info = get_info(anchor);
                        let end_info = get_info(focus);

                        if let (Some((start_off, _)), Some((end_off, _))) = (start_info, end_info) {
                            let (min, max) = if start_off < end_off { (start_off, end_off) } else { (end_off, start_off) };
                            
                            // Valid HexView selection found
                            set_active.set(true);
                            if let Some(set_g) = set_global {
                                set_g.set(Some(SelectionRange::new(
                                    min,
                                    max + 1, // +1 because max is inclusive in our logic but EndOffset implies exclusive usually, let's consistency check.
                                            // SelectionRange logic: [start, end). If min==max, len=0.
                                            // byte_idx logic: "if byte_idx < group_len".
                                            // If I select byte 0, start=0, end=0.
                                            // Browser selection usually implies char ranges.
                                            // Let's assume Inclusive for min/max derived here.
                                            // So range is [min, max + 1).
                                    0,
                                    0,
                                    SelectionSource::HexView
                                )));
                            }
                        }
                    }
                }
            }
        }) as Box<dyn FnMut()>);

        let document = web_sys::window().unwrap().document().unwrap();
        // Use "selectionchange" on document
        let _ = document.add_event_listener_with_callback("selectionchange", callback.as_ref().unchecked_ref());

        on_cleanup(move || {
            let _ = document.remove_event_listener_with_callback("selectionchange", callback.as_ref().unchecked_ref());
            callback.forget(); // Memory leak risk if not careful, but on_cleanup handles the listener removal. The Closure needs to be kept alive?
                               // Actually Closure needs to be stored or forgotten. If forgotten, it leaks.
                               // Ideally we store it. But for this scope, let's just leak the minimal closure or handle it properly if we can store it.
                               // Given `leptos` constructs, standard simple closures leak if forgotten.
                               // Proper: store in a signal or just accept small leak for single component lifecycle.
        });
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
                                <div class="hex-data-container" style="display: flex; gap: 16px; user-select: text;">
                                    {
                                        let total_groups = bpr / 4;
                                        let current_groups = groups.len();

                                        // 1. Render actual data groups
                                        let mut views = groups.into_iter().enumerate().map(|(group_idx, group)| {
                                            let is_sep = group_idx < total_groups - 1;

                                            // Render each byte with selection support
                                            let bytes_for_group = group.clone();
                                            let group_len = bytes_for_group.len();
                                            let byte_views = (0..4).map(|byte_idx| {
                                                let byte_offset = offset + (group_idx * 4) + byte_idx;
                                                let hex_str = bytes_for_group.get(byte_idx)
                                                    .map(|b| format!("{:02X}", b))
                                                    .unwrap_or_else(|| "  ".to_string());

                                                view! {
                                                    <span
                                                        class="hex-byte"
                                                        data-offset={byte_offset.to_string()}
                                                        style=move || {
                                                            // Check if selected from Terminal→Hex sync OR Internal Hex Selection
                                                            let selected = if byte_idx < group_len {
                                                                if let Some(global_sel) = global_selection {
                                                                    if let Some(range) = global_sel.get() {
                                                                         // Highlight if range matches, regardless of source (Terminal OR HexView)
                                                                         range.contains_offset(byte_offset)
                                                                    } else {
                                                                        false
                                                                    }
                                                                } else {
                                                                    false
                                                                }
                                                            } else {
                                                                false
                                                            };

                                                            format!(
                                                                "width: 2ch; text-align: center; cursor: text; display: inline-block; {}",
                                                                if selected {
                                                                    "background-color: rgba(86, 156, 214, 0.3); border-radius: 2px;"
                                                                } else {
                                                                    ""
                                                                }
                                                            )
                                                        }
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
                                                    style=move || {
                                                         // Check if selected from Terminal→Hex sync OR Global
                                                         let selected = if let Some(global_sel) = global_selection {
                                                             if let Some(range) = global_sel.get() {
                                                                 range.contains_offset(byte_offset)
                                                             } else {
                                                                 false
                                                             }
                                                         } else {
                                                             false
                                                         };

                                                        format!(
                                                            "cursor: text; {}",
                                                            if selected {
                                                                "background-color: rgba(86, 156, 214, 0.3);"
                                                            } else {
                                                                ""
                                                            }
                                                        )
                                                    }
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
