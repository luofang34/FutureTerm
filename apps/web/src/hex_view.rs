use leptos::*;
use core_types::{DecodedEvent, Value};
use wasm_bindgen::JsCast;

#[component]
pub fn HexView(
    events: ReadSignal<Vec<DecodedEvent>>
) -> impl IntoView {
    let container_ref = create_node_ref::<html::Div>();
    
    // Auto-scroll logic
    create_effect(move |_| {
        events.with(|_| {}); // Track changes
        if let Some(div) = container_ref.get() {
            // Simple auto-scroll: If we were at bottom, stay at bottom? 
            // For now, just forced scroll to bottom like terminal for "live" feel
            div.set_scroll_top(div.scroll_height());
        }
    });

    view! {
        <div 
            _ref=container_ref
            class="hex-view"
            style="
                width: 100%; 
                height: 100%; 
                background: #1e1e1e; 
                color: #d4d4d4; 
                font-family: 'Menlo', 'Monaco', monospace; 
                font-size: 13px; 
                overflow-y: auto;
                padding: 10px;
                box-sizing: border-box;
            "
        >
            <div style="display: grid; grid-template-columns: 80px 1fr 1fr; gap: 10px; color: #569cd6; font-weight: bold; margin-bottom: 8px; border-bottom: 1px solid #333; padding-bottom: 4px;">
                <div>OFFSET</div>
                <div>HEX REPRESENTATION</div>
                <div>ASCII</div>
            </div>
            
            <For
                each=move || events.get()
                key=|evt| evt.timestamp_us // Use unique timestamp
                children=move |evt: DecodedEvent| {
                    // Extract fields if available (from HexDecoder)
                    let hex_val = evt.get_field("hex").and_then(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        _ => None
                    }).unwrap_or_else(|| evt.summary.to_string()); // Fallback
                    
                    let ascii_val = evt.get_field("ascii").and_then(|v| match v {
                        Value::String(s) => Some(s.clone()),
                        _ => None
                    }).unwrap_or_else(|| ".".to_string());

                    // Calculate a virtual offset based on timestamp (pseudo) or just sequence
                    // Real offset would require accumulating bytes. 
                    // For now, use timestamp last 4 digits as "virtual ID"
                    let offset = (evt.timestamp_us % 10000).to_string();

                    view! {
                        <div style="display: grid; grid-template-columns: 80px 1fr 1fr; gap: 10px; border-bottom: 1px solid #2d2d2d; padding: 2px 0; align-items: start;">
                            <div style="color: #858585;">{format!("{:04}", offset)}</div>
                            <div style="color: #ce9178; word-break: break-all;">{hex_val}</div>
                            <div style="color: #b5cea8; white-space: pre-wrap; word-break: break-all;">{ascii_val}</div>
                        </div>
                    }
                }
            />
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
