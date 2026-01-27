use leptos::*;
use std::fmt;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::HtmlDivElement;

use crate::terminal_metadata::TerminalMetadata;
use core_types::{SelectionRange, SelectionSource};

#[wasm_bindgen]
extern "C" {
    // --- Terminal ---
    #[wasm_bindgen(extends = js_sys::Object)]
    pub type Terminal;

    #[wasm_bindgen(constructor, js_namespace = window)]
    pub fn new(options: Option<js_sys::Object>) -> Terminal;

    #[wasm_bindgen(method)]
    pub fn open(this: &Terminal, parent: &HtmlDivElement);

    #[wasm_bindgen(method)]
    pub fn write(this: &Terminal, data: &str);

    #[wasm_bindgen(method)]
    pub fn clear(this: &Terminal);

    #[wasm_bindgen(method, js_name = onData)]
    pub fn on_data(this: &Terminal, callback: js_sys::Function);

    // Selection API
    #[wasm_bindgen(method, js_name = onSelectionChange)]
    pub fn on_selection_change(this: &Terminal, callback: js_sys::Function);

    #[wasm_bindgen(method, js_name = getSelection)]
    pub fn get_selection(this: &Terminal) -> String;

    #[wasm_bindgen(method, js_name = getSelectionPosition)]
    pub fn get_selection_position(this: &Terminal) -> JsValue;

    #[wasm_bindgen(method, js_name = clearSelection)]
    pub fn clear_selection(this: &Terminal);

    #[wasm_bindgen(method, js_name = select)]
    pub fn select(this: &Terminal, column: u32, row: u32, length: u32);

    // Alternative: selectLines(start, end) if we wanted line-only
    // But select() is robust. Wait, xterm.js API has select(col, row, len).
    // It implies single line?
    // "Selects text within the terminal."
    // Actually xterm.js 5.3.0 has `select(column, row, length)`.
    // It does NOT support multi-line selection via parameters directly?
    // "Selects text in the buffer. The selection is always treated as a single block."
    // No, standard xterm selection can span lines.
    // Documentation says: `select(column: number, row: number, length: number): void`
    // This looks like single line.
    // But `selectAll()` exists.
    // What about `selectLines(start, end)`? "Selects all text within the specified lines."
    // Let's use `selectLines` as a fallback if full range is complex.
    // Or check if there is `selectRange`.
    // Actually, `select` with very long length wraps lines!
    // So we can calculate length? length = (end_row - start_row) * cols + (end_col - start_col).
    // But we don't know "cols" (width) reliably inside metadata easily.
    // Let's check imports.

    // xterm.js also has `selectLines(start, end)`.
    #[wasm_bindgen(method, js_name = selectLines)]
    pub fn select_lines(this: &Terminal, start: u32, end: u32);

    #[wasm_bindgen(method, js_name = hasSelection)]
    pub fn has_selection(this: &Terminal) -> bool;

    // CHANGED: Accept JsValue for addon to support manual instantiation
    #[wasm_bindgen(method, js_name = loadAddon)]
    pub fn load_addon(this: &Terminal, addon: &JsValue);

    // Decorations API
    #[wasm_bindgen(method, js_name = registerDecoration)]
    pub fn register_decoration(this: &Terminal, options: &JsValue) -> JsValue;

    #[wasm_bindgen(method, js_name = registerMarker)]
    pub fn register_marker(this: &Terminal, cursor_y_offset: i32) -> JsValue;

    // Scrolling API
    #[wasm_bindgen(method, js_name = scrollLines)]
    pub fn scroll_lines(this: &Terminal, amount: i32);

    // Buffer access
    #[wasm_bindgen(method, getter)]
    pub fn buffer(this: &Terminal) -> JsValue;
}

// ISelectionPosition interface
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(extends = js_sys::Object)]
    pub type SelectionPosition;

    #[wasm_bindgen(method, getter, js_name = startColumn)]
    pub fn start_column(this: &SelectionPosition) -> u32;

    #[wasm_bindgen(method, getter, js_name = startRow)]
    pub fn start_row(this: &SelectionPosition) -> u32;

    #[wasm_bindgen(method, getter, js_name = endColumn)]
    pub fn end_column(this: &SelectionPosition) -> u32;

    #[wasm_bindgen(method, getter, js_name = endRow)]
    pub fn end_row(this: &SelectionPosition) -> u32;
}

// IDecoration interface
#[wasm_bindgen]
extern "C" {
    #[wasm_bindgen(extends = js_sys::Object)]
    pub type Decoration;

    #[wasm_bindgen(method)]
    pub fn dispose(this: &Decoration);

    #[wasm_bindgen(method, getter)]
    pub fn marker(this: &Decoration) -> JsValue;

    #[wasm_bindgen(method, getter)]
    pub fn element(this: &Decoration) -> web_sys::HtmlElement;
}

// Manual Clone/PartialEq implementations
impl Clone for Terminal {
    fn clone(&self) -> Self {
        self.unchecked_ref::<JsValue>().clone().unchecked_into()
    }
}
impl PartialEq for Terminal {
    fn eq(&self, other: &Self) -> bool {
        self.unchecked_ref::<JsValue>() == other.unchecked_ref::<JsValue>()
    }
}

#[derive(Clone)]
pub struct TerminalHandle(pub Terminal);

impl fmt::Debug for TerminalHandle {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TerminalHandle").finish()
    }
}

impl PartialEq for TerminalHandle {
    fn eq(&self, other: &Self) -> bool {
        self.0 == other.0
    }
}

impl TerminalHandle {
    pub fn write(&self, data: &str) {
        self.0.write(data);
    }
    #[allow(dead_code)]
    pub fn clear(&self) {
        self.0.clear();
    }
    pub fn on_data(&self, callback: js_sys::Function) {
        self.0.on_data(callback);
    }

    // Selection API wrappers (reserved for future cross-view selection sync)
    #[allow(dead_code)]
    pub fn on_selection_change(&self, callback: js_sys::Function) {
        self.0.on_selection_change(callback);
    }

    #[allow(dead_code)]
    pub fn get_selection(&self) -> String {
        self.0.get_selection()
    }

    #[allow(dead_code)]
    pub fn get_selection_position(&self) -> JsValue {
        self.0.get_selection_position()
    }

    #[allow(dead_code)]
    pub fn clear_selection(&self) {
        self.0.clear_selection();
    }

    #[allow(dead_code)]
    pub fn has_selection(&self) -> bool {
        self.0.has_selection()
    }

    // Parse selection position into tuple
    // NOTE: xterm.js returns {start: {x, y}, end: {x, y}} structure
    #[allow(dead_code)]
    pub fn get_selection_position_parsed(&self) -> Option<(u32, u32, u32, u32)> {
        let pos = self.0.get_selection_position();
        if pos.is_undefined() || pos.is_null() {
            return None;
        }

        // Try flattened format first (startColumn, startRow...) - Common in older/some bindings
        let start_col = js_sys::Reflect::get(&pos, &"startColumn".into()).ok();
        if let Some(sc) = start_col.and_then(|v| v.as_f64()) {
            let start_row = js_sys::Reflect::get(&pos, &"startRow".into())
                .ok()
                .and_then(|v| v.as_f64())?;
            let end_col = js_sys::Reflect::get(&pos, &"endColumn".into())
                .ok()
                .and_then(|v| v.as_f64())?;
            let end_row = js_sys::Reflect::get(&pos, &"endRow".into())
                .ok()
                .and_then(|v| v.as_f64())?;
            return Some((start_row as u32, sc as u32, end_row as u32, end_col as u32));
        }

        // Try nested format ({ start: { x, y } }) - Newer API
        // Structure: {start: {x: col, y: row}, end: {x: col, y: row}}
        let start = js_sys::Reflect::get(&pos, &"start".into()).ok()?;
        if !start.is_undefined() {
            let start_x = js_sys::Reflect::get(&start, &"x".into()).ok()?;
            let start_y = js_sys::Reflect::get(&start, &"y".into()).ok()?;

            let end = js_sys::Reflect::get(&pos, &"end".into()).ok()?;
            let end_x = js_sys::Reflect::get(&end, &"x".into()).ok()?;
            let end_y = js_sys::Reflect::get(&end, &"y".into()).ok()?;

            let start_col = start_x.as_f64()? as u32;
            let start_row = start_y.as_f64()? as u32;
            // Note: end_x in some versions is inclusive, some exclusive.
            // xterm.js usually implies range [start, end].
            // We'll trust the values.
            let end_col = end_x.as_f64()? as u32;
            let end_row = end_y.as_f64()? as u32;

            return Some((start_row, start_col, end_row, end_col));
        }

        None
    }

    // Decorations API
    #[allow(dead_code)]
    pub fn register_decoration(&self, options: &JsValue) -> Option<Decoration> {
        let result = self.0.register_decoration(options);
        if result.is_undefined() || result.is_null() {
            None
        } else {
            Some(result.unchecked_into())
        }
    }

    #[allow(dead_code)]
    pub fn register_marker(&self, cursor_y_offset: i32) -> JsValue {
        self.0.register_marker(cursor_y_offset)
    }

    // Scrolling
    #[allow(dead_code)]
    pub fn scroll_lines(&self, amount: i32) {
        self.0.scroll_lines(amount);
    }

    // Buffer access
    #[allow(dead_code)]
    pub fn buffer(&self) -> JsValue {
        self.0.buffer()
    }
}

// Helper to manually fit terminal using the addon instance
fn fit_terminal(addon: &JsValue) {
    if let Ok(fit_fn) = js_sys::Reflect::get(addon, &"fit".into()) {
        if let Ok(fit_fn) = fit_fn.dyn_into::<js_sys::Function>() {
            let _ = fit_fn.call0(addon);
        }
    }
}

#[component]
pub fn TerminalView(
    #[prop(optional)] on_mount: Option<Callback<()>>,
    #[prop(optional)] on_terminal_ready: Option<Callback<TerminalHandle>>,
    #[prop(optional)] terminal_metadata: Option<ReadSignal<TerminalMetadata>>,
    #[prop(optional)] global_selection: Option<ReadSignal<Option<SelectionRange>>>,
    #[prop(optional)] set_global_selection: Option<WriteSignal<Option<SelectionRange>>>,
) -> impl IntoView {
    let div_ref = create_node_ref::<html::Div>();

    // Internal signal to share terminal handle with other effects
    let (internal_term_handle, set_internal_term_handle) =
        create_signal::<Option<TerminalHandle>>(None);

    let on_mount_clone = on_mount;
    let on_terminal_ready_clone = on_terminal_ready;

    create_effect(move |_| {
        if let Some(div) = div_ref.get() {
            // Options: Set Theme
            let options = js_sys::Object::new();
            let theme = js_sys::Object::new();
            // CHANGED: Match background to container (rgb(25,25,25) -> #191919)
            let _ = js_sys::Reflect::set(&theme, &"background".into(), &"#191919".into());
            let _ = js_sys::Reflect::set(&options, &"theme".into(), &theme);

            // Standard config
            let _ = js_sys::Reflect::set(&options, &"cursorBlink".into(), &true.into());
            let _ = js_sys::Reflect::set(&options, &"fontSize".into(), &14.into());
            let _ = js_sys::Reflect::set(
                &options,
                &"fontFamily".into(),
                &"Menlo, Monaco, 'Courier New', monospace".into(),
            );

            // Initialize xterm with options
            let term = Terminal::new(Some(options));

            // Initialize FitAddon manually via Reflection (bypassing wasm_bindgen macro issues)
            let mut fit_addon_instance: Option<JsValue> = None;

            if let Some(window) = web_sys::window() {
                // Access window.FitAddon (Object/Module)
                if let Ok(fa_module) = js_sys::Reflect::get(&window, &"FitAddon".into()) {
                    // Access window.FitAddon.FitAddon (Constructor)
                    if let Ok(fa_class) = js_sys::Reflect::get(&fa_module, &"FitAddon".into()) {
                        if let Ok(fa_ctor) = fa_class.dyn_into::<js_sys::Function>() {
                            if let Ok(instance) =
                                js_sys::Reflect::construct(&fa_ctor, &js_sys::Array::new())
                            {
                                term.load_addon(&instance);
                                fit_addon_instance = Some(instance);
                            }
                        }
                    }
                }
            }

            // Convert Leptos HtmlElement to web_sys::HtmlDivElement
            // Clone the inner HtmlDivElement (via Deref) before casting
            let div_element: HtmlDivElement =
                <HtmlDivElement as Clone>::clone(&div).unchecked_into();

            term.open(&div_element);

            // Defer fit() to ensure layout is ready
            if let Some(fa) = fit_addon_instance {
                // Initial deferred fit
                let fa_clone1 = fa.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let _ =
                        wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(&mut |r, _| {
                            if let Some(window) = web_sys::window() {
                                let _ = window
                                    .set_timeout_with_callback_and_timeout_and_arguments_0(&r, 10);
                            }
                        }))
                        .await;
                    fit_terminal(&fa_clone1);
                });

                // Add resize listener
                if let Some(window) = web_sys::window() {
                    let fa_clone2 = fa.clone();
                    let on_resize = Closure::wrap(Box::new(move || {
                        fit_terminal(&fa_clone2);
                    }) as Box<dyn FnMut()>);

                    let _ = window.add_event_listener_with_callback(
                        "resize",
                        on_resize.as_ref().unchecked_ref(),
                    );

                    // Cleanup listener when scope is dropped
                    on_cleanup(move || {
                        // Note: We need window here again.
                        // To be perfectly safe we should clone window or look it up.
                        if let Some(w) = web_sys::window() {
                            let _ = w.remove_event_listener_with_callback(
                                "resize",
                                on_resize.as_ref().unchecked_ref(),
                            );
                        }
                    });
                }
            }

            // Store terminal handle for effects
            let term_handle = TerminalHandle(term.clone());

            // Setup Terminal → Hex selection sync
            if let (Some(metadata_signal), Some(set_global_sel)) =
                (terminal_metadata, set_global_selection)
            {
                let term_clone = term.clone();
                let selection_callback = Closure::<dyn Fn()>::new(move || {
                    let handle = TerminalHandle(term_clone.clone());

                    // Enhanced debug: Check if selection exists
                    let has_sel = handle.has_selection();
                    let sel_text = handle.get_selection();
                    web_sys::console::log_1(
                        &format!(
                            "Terminal onSelectionChange fired: has_selection={}, text_length={}",
                            has_sel,
                            sel_text.len()
                        )
                        .into(),
                    );

                    // Get raw position value for debugging
                    let raw_pos = handle.get_selection_position();
                    web_sys::console::log_2(&"Raw selection position:".into(), &raw_pos);

                    // Get selection position
                    if let Some((start_row, start_col, end_row, end_col)) =
                        handle.get_selection_position_parsed()
                    {
                        // Debug logging with all coordinates and selected text
                        web_sys::console::log_1(
                            &format!(
                                "Terminal selection: rows {}-{}, cols {}-{}, selected_text={:?}",
                                start_row, end_row, start_col, end_col, sel_text
                            )
                            .into(),
                        );

                        // Map Terminal position (row+col) to byte range via metadata
                        let meta = metadata_signal.get_untracked();
                        web_sys::console::log_1(
                            &format!("Metadata span count: {}", meta.span_count()).into(),
                        );

                        if let Some((byte_start, byte_end)) = meta.terminal_position_to_bytes(
                            start_row as usize,
                            start_col as usize,
                            end_row as usize,
                            end_col as usize,
                        ) {
                            web_sys::console::log_1(
                                &format!("Mapped to bytes: {}-{}", byte_start, byte_end).into(),
                            );

                            // Create selection range
                            // Note: Terminal selections use byte offsets only.
                            // Timestamp fields (start/end) are set to 0 as terminal
                            // displays current buffer state without historical timing.
                            let range = SelectionRange::new(
                                byte_start,
                                byte_end,
                                0, // timestamp_start_us
                                0, // timestamp_end_us
                                SelectionSource::Terminal,
                            );
                            set_global_sel.set(Some(range));
                        } else {
                            web_sys::console::log_1(
                                &"Failed to map terminal lines to bytes".into(),
                            );
                        }
                    } else {
                        // Selection cleared
                        web_sys::console::log_1(&"Selection position is None (cleared)".into());
                        set_global_sel.set(None);
                    }
                });

                term.on_selection_change(selection_callback.into_js_value().unchecked_into());
            }

            // Store handle in signal for other effects
            set_internal_term_handle.set(Some(term_handle.clone()));

            if let Some(cb) = on_terminal_ready_clone {
                cb.call(term_handle);
            }

            if let Some(cb) = on_mount_clone {
                cb.call(());
            }
        }
    });

    // Setup Hex → Terminal highlighting with decorations
    if let (Some(metadata_signal), Some(global_sel)) = (terminal_metadata, global_selection) {
        create_effect(move |prev_decoration: Option<Option<Decoration>>| {
            let current_decoration: Option<Decoration> = if let Some(range) = global_sel.get() {
                if range.source_view == SelectionSource::HexView {
                    web_sys::console::log_1(
                        &format!(
                            "Hex selection: bytes {}-{}",
                            range.start_byte_offset, range.end_byte_offset
                        )
                        .into(),
                    );

                    // HexView selected bytes, highlight in Terminal
                    let meta = metadata_signal.get_untracked();
                    let term_handle = internal_term_handle.get()?;

                    if let Some((start_row, start_col, end_row, end_col)) = meta
                        .bytes_to_terminal_position(range.start_byte_offset, range.end_byte_offset)
                    {
                        web_sys::console::log_1(
                            &format!(
                                "Mapped to position: ({}, {}) - ({}, {})",
                                start_row, start_col, end_row, end_col
                            )
                            .into(),
                        );

                        let start_u32 = start_row as u32;
                        let end_u32 = end_row as u32;

                        term_handle.0.select_lines(start_u32, end_u32);
                    }
                }

                None
            } else {
                None
            };

            // Dispose previous decoration if it exists
            if let Some(Some(old_decoration)) = prev_decoration {
                old_decoration.dispose();
            }

            current_decoration
        });
    }

    view! {
        <div style="width: 100%; height: 100%; background: #191919; padding: 10px 10px 0 10px; box-sizing: border-box; position: relative;">
            <div _ref=div_ref style="width: 100%; height: 100%; overflow: hidden;" />
        </div>
    }
}

pub fn icon() -> impl IntoView {
    view! {
        <svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
           <rect x="3" y="3" width="18" height="18" rx="2" ry="2" />
           <path d="M8 8l4 4l-4 4" />
           <path d="M13 16h4" />
        </svg>
    }
}
