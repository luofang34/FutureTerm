use leptos::*;
use wasm_bindgen::prelude::*;
use wasm_bindgen::JsCast;
use web_sys::HtmlDivElement;

use crate::terminal_metadata::TerminalMetadata;
use core_types::{SelectionRange, SelectionSource};

use super::{Decoration, Terminal, TerminalHandle};

fn fit_terminal(addon: &JsValue) {
    if let Ok(fit_fn) = js_sys::Reflect::get(addon, &"fit".into()) {
        if let Ok(fit_fn) = fit_fn.dyn_into::<js_sys::Function>() {
            let _ = fit_fn.call0(addon);
        }
    }
}

const MIN_TERMINAL_COLS: u32 = 80;
const DEFAULT_FONT_SIZE: f64 = 14.0;
const MIN_FONT_SIZE: f64 = 6.0;

/// Fit terminal, scaling font size down on narrow screens to maintain minimum columns.
/// On wider screens, restores the default font size if it was previously scaled down.
fn fit_terminal_with_scaling(addon: &JsValue, term: &Terminal) {
    // First pass: fit with current font size
    fit_terminal(addon);

    let cols = term.cols();

    if cols < MIN_TERMINAL_COLS && cols > 0 {
        // Container too narrow for 80 cols at current font; scale down
        let new_size =
            (DEFAULT_FONT_SIZE * (cols as f64 / MIN_TERMINAL_COLS as f64)).max(MIN_FONT_SIZE);

        if let Ok(options) = js_sys::Reflect::get(term, &"options".into()) {
            let _ = js_sys::Reflect::set(&options, &"fontSize".into(), &new_size.into());
        }
        // Re-fit with smaller font to actually get >= 80 cols
        fit_terminal(addon);
    } else if cols >= MIN_TERMINAL_COLS {
        // Enough room; restore default font if it was scaled down
        if let Ok(options) = js_sys::Reflect::get(term, &"options".into()) {
            if let Ok(current_size_val) = js_sys::Reflect::get(&options, &"fontSize".into()) {
                if let Some(current_size) = current_size_val.as_f64() {
                    if (current_size - DEFAULT_FONT_SIZE).abs() > 0.01 {
                        let _ = js_sys::Reflect::set(
                            &options,
                            &"fontSize".into(),
                            &DEFAULT_FONT_SIZE.into(),
                        );
                        fit_terminal(addon);
                    }
                }
            }
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
    let outer_ref = create_node_ref::<html::Div>();

    // Internal signal to share terminal handle with other effects
    let (internal_term_handle, set_internal_term_handle) =
        create_signal::<Option<TerminalHandle>>(None);

    // Guard flag to prevent feedback loop when hex view sets terminal selection programmatically
    let (is_programmatic_select, set_is_programmatic_select) = create_signal(false);

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
                // Initial deferred fit with scaling
                let fa_clone1 = fa.clone();
                let term_clone1 = term.clone();
                wasm_bindgen_futures::spawn_local(async move {
                    let _ =
                        wasm_bindgen_futures::JsFuture::from(js_sys::Promise::new(&mut |r, _| {
                            if let Some(window) = web_sys::window() {
                                let _ = window
                                    .set_timeout_with_callback_and_timeout_and_arguments_0(&r, 10);
                            }
                        }))
                        .await;
                    fit_terminal_with_scaling(&fa_clone1, &term_clone1);
                });

                // Setup ResizeObserver on outer container div for re-fit on visibility/size changes
                if let Some(outer_div) = outer_ref.get() {
                    let fa_clone2 = fa.clone();
                    let term_clone2 = term.clone();

                    let callback = Closure::wrap(Box::new(move |entries: js_sys::Array| {
                        // Check that container has real dimensions before fitting.
                        // When parent is display:none, ResizeObserver fires with 0x0
                        // and calling fit() on a zero-size container corrupts xterm state.
                        for entry in entries.iter() {
                            if let Ok(entry) = entry.dyn_into::<web_sys::ResizeObserverEntry>() {
                                let rect = entry.content_rect();
                                if rect.width() > 0.0 && rect.height() > 0.0 {
                                    fit_terminal_with_scaling(&fa_clone2, &term_clone2);
                                }
                            }
                        }
                    })
                        as Box<dyn FnMut(js_sys::Array)>);

                    if let Ok(observer) =
                        web_sys::ResizeObserver::new(callback.as_ref().unchecked_ref())
                    {
                        observer.observe(&outer_div);

                        let observer_clone = observer.clone();
                        on_cleanup(move || {
                            observer_clone.disconnect();
                        });

                        // Intentionally keep callback alive for observer lifetime
                        callback.forget();
                    }
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
                    // Skip selection changes triggered by programmatic hex → terminal sync
                    if is_programmatic_select.get_untracked() {
                        set_is_programmatic_select.set(false);
                        return;
                    }

                    let handle = TerminalHandle(term_clone.clone());

                    let sel_text = handle.get_selection();

                    #[cfg(debug_assertions)]
                    {
                        let has_sel = handle.has_selection();
                        web_sys::console::log_1(
                            &format!(
                                "Terminal onSelectionChange fired: has_selection={}, text_length={}",
                                has_sel,
                                sel_text.len()
                            )
                            .into(),
                        );

                        let raw_pos = handle.get_selection_position();
                        web_sys::console::log_2(&"Raw selection position:".into(), &raw_pos);
                    }

                    // Get selection position
                    if let Some((start_row, start_col, end_row, end_col)) =
                        handle.get_selection_position_parsed()
                    {
                        #[cfg(debug_assertions)]
                        {
                            web_sys::console::log_1(
                                &format!(
                                    "Terminal selection: rows {}-{}, cols {}-{}, selected_text={:?}",
                                    start_row, end_row, start_col, end_col, sel_text
                                )
                                .into(),
                            );
                        }

                        // Map Terminal position (row+col) to byte range via metadata
                        let meta = metadata_signal.get_untracked();
                        #[cfg(debug_assertions)]
                        web_sys::console::log_1(
                            &format!("Metadata span count: {}", meta.span_count()).into(),
                        );

                        if let Some((byte_start, byte_end)) = meta.terminal_position_to_bytes(
                            start_row as usize,
                            start_col as usize,
                            end_row as usize,
                            end_col as usize,
                        ) {
                            #[cfg(debug_assertions)]
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
                            #[cfg(debug_assertions)]
                            web_sys::console::log_1(
                                &"Failed to map terminal lines to bytes".into(),
                            );
                        }
                    } else {
                        // Selection cleared
                        #[cfg(debug_assertions)]
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
                    #[cfg(debug_assertions)]
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
                        #[cfg(debug_assertions)]
                        web_sys::console::log_1(
                            &format!(
                                "Mapped to position: ({}, {}) - ({}, {})",
                                start_row, start_col, end_row, end_col
                            )
                            .into(),
                        );

                        // Set guard before programmatic selection to prevent feedback loop
                        set_is_programmatic_select.set(true);

                        // Use column-precise select() instead of select_lines()
                        // length = (delta_rows * cols) + end_col - start_col
                        // Evaluation order (left-to-right) ensures no usize underflow
                        let cols = term_handle.0.cols() as usize;
                        if cols > 0 {
                            let length = (end_row - start_row) * cols + end_col - start_col;
                            if length > 0 {
                                term_handle.0.select(
                                    start_col as u32,
                                    start_row as u32,
                                    length as u32,
                                );
                            }
                        }
                    }
                }

                None
            } else {
                // Selection cleared, remove terminal programmatic highlight
                if let Some(term_handle) = internal_term_handle.get() {
                    set_is_programmatic_select.set(true);
                    term_handle.0.clear_selection();
                }
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
        <div _ref=outer_ref style="width: 100%; height: 100%; background: #191919; padding: 10px 10px 0 10px; box-sizing: border-box; position: relative;">
            <div _ref=div_ref style="width: 100%; height: 100%; overflow: hidden;" />
        </div>
    }
}
