use leptos::*;
use std::fmt;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::HtmlDivElement;

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

    #[wasm_bindgen(method, getter)]
    pub fn cols(this: &Terminal) -> u32;

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
pub fn icon() -> impl IntoView {
    view! {
        <svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">
           <rect x="3" y="3" width="18" height="18" rx="2" ry="2" />
           <path d="M8 8l4 4l-4 4" />
           <path d="M13 16h4" />
        </svg>
    }
}

mod view;
pub use view::TerminalView;
