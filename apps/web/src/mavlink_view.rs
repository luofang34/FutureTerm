use core_types::DecodedEvent;
use leptos::*;
use std::collections::BTreeMap;

#[component]
pub fn MavlinkView(events_list: ReadSignal<Vec<DecodedEvent>>) -> impl IntoView {
    // Data structures for the view
    #[derive(Clone, PartialEq, Debug)]
    struct SystemGroup {
        sys_id: i64,
        comp_id: i64,
        heartbeat: Option<DecodedEvent>,
        messages: Vec<DecodedEvent>,
    }

    // 1. Persistent State for the Dashboard
    // We maintain a map of (sys_id, comp_id) -> (Heartbeat, Map<MsgName, Event>)
    // This accumulates data and never drops it, solving the flickering issue.
    // The inner map stores the LATEST event for each message type.
    type SystemState = BTreeMap<(i64, i64), (Option<DecodedEvent>, BTreeMap<String, DecodedEvent>)>;
    let (state, set_state) = create_signal::<SystemState>(BTreeMap::new());

    // Effect: Sync events to state
    // We scan the entire event buffer (max 2500) on update. 
    // This is cheap (O(N) * log(M)) and much faster than rebuilding the map (O(N) allocs).
    // We iterate forward (Old -> New) to naturally let newer events overwrite older ones.
    create_effect(move |_| {
        events_list.with(|events| {
            if events.is_empty() {
                return;
            }
            // debug log
            web_sys::console::log_1(&format!("View Processing {} events", events.len()).into());

            set_state.update(|map| {
                for e in events {
                    if e.protocol != "MAVLink" {
                        continue;
                    }

                    let mut sys_id = 0;
                    let mut comp_id = 0;
                    for (k, v) in &e.fields {
                        if k == "sys_id" {
                            if let core_types::Value::I64(val) = v {
                                sys_id = *val;
                            }
                        } else if k == "comp_id" {
                            if let core_types::Value::I64(val) = v {
                                comp_id = *val;
                            }
                        }
                    }

                    let entry = map
                        .entry((sys_id, comp_id))
                        .or_insert((None, BTreeMap::new()));

                    if e.summary == "HEARTBEAT" {
                        // Forward iteration: Newer overwrites older automatically if we just set it.
                        // But we verify timestamps just to be safe against buffer weirdness.
                        if let Some(existing) = &entry.0 {
                            if e.timestamp_us >= existing.timestamp_us {
                                entry.0 = Some(e.clone());
                            }
                        } else {
                            entry.0 = Some(e.clone());
                        }
                    } else {
                        // Standard Message
                        match entry.1.get(e.summary.as_ref()) {
                            Some(existing) => {
                                if e.timestamp_us >= existing.timestamp_us {
                                    entry.1.insert(e.summary.to_string(), e.clone());
                                }
                            },
                            None => {
                                entry.1.insert(e.summary.to_string(), e.clone());
                            }
                        }
                    }
                }
            });
        });
    });

    // 2. Systems Map Helper (Read Access)
    let systems_map = state;

    // 2. Derive the list of Keys (stable identifiers)
    // This allows the <For> to only create rows when new systems appear, not on every update.
    let system_keys =
        create_memo(move |_| systems_map.with(|m| m.keys().cloned().collect::<Vec<_>>()));

    // CSS
    let style = r#"
        @keyframes flash-green { 0% { background-color: rgba(76, 175, 80, 0.8); } 100% { background-color: transparent; } }
        .flash-update {
            animation: flash-green 0.5s ease-out; width: 10px; height: 10px; border-radius: 50%; display: inline-block; margin-right: 8px;
        }
        .sys-card {
            background: #2b2b2b; border: 1px solid #444; border-radius: 6px; padding: 10px; margin-right: 10px; 
            width: fit-content; min-width: 250px; max-width: 100%; height: fit-content;
            display: flex; flex-direction: column; gap: 5px;
            font-family: system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, Oxygen, Ubuntu, Cantarell, "Open Sans", "Helvetica Neue", sans-serif;
        }
        .sys-card-header { display: flex; align-items: center; gap: 8px; font-weight: 600; color: #eee; border-bottom: 1px solid #444; padding-bottom: 8px; font-size: 1.1em; letter-spacing: 0.5px; }
        .sys-info-row { display: flex; justify-content: flex-start; align-items: flex-start; font-size: 0.85em; flex-wrap: wrap; gap: 8px; padding-top: 4px; border-bottom: 1px solid #333; padding-bottom: 4px; }
        .sys-info-row:last-child { border-bottom: none; }
        .sys-label { color: #9E9E9E; font-weight: 600; white-space: nowrap; min-width: 70px; }
        // Force wrapping for long values (Mode), right aligned usually but wraps if needed
        .sys-val { color: #64B5F6; font-family: "SFMono-Regular", Consolas, "Liberation Mono", Menlo, monospace; font-size: 0.9em; text-align: left; white-space: pre-wrap; overflow-wrap: anywhere; max-width: 100%; width: auto; display: flex; flex-direction: column; align-items: flex-start; }
        .group-header { background: #1e1e1e; color: #888; font-weight: bold; padding: 8px; border-bottom: 1px solid #333; margin-top: 10px; }
        .msg-table { width: 100%; border-collapse: collapse; font-family: monospace; font-size: 0.9em; }
        .msg-row { border-bottom: 1px solid #333; }
        .msg-cell { padding: 5px; vertical-align: top; color: #ccc; }
    "#;

    view! {

        <style>{style}</style>
        <div style="display: flex; flex-direction: column; width: 100%; height: 100%; padding: 10px; overflow: hidden;">


            // 1. Heartbeat Header Section
            <div style="flex: 0 0 auto; display: flex; flex-wrap: wrap; gap: 10px; margin-bottom: 15px;">
                <For
                    each=move || system_keys.get()
                    key=|k| *k
                    children=move |key| {
                        // Create a reactive slice for this specific system
                        let system_data = create_memo(move |_| {
                            systems_map.with(|m| {
                                m.get(&key).and_then(|(hb, _)| hb.clone())
                            })
                        });

                        view! {
                             // Only render if we have a heartbeat
                             {move || match system_data.get() {
                                 Some(hb) => {
                                     let get_str = |k: &str| {
                                         let val = hb.fields.iter().find(|(f, _)| f == k).map(|(_, v)| v.to_string()).unwrap_or_default();
                                         // Clean up verbose MAVLink enums
                                         val.replace("MAV_TYPE_", "")
                                            .replace("MAV_MODE_FLAG_", "")
                                            .replace("MAV_STATE_", "")
                                            .replace(" | ", "\n")
                                            .replace("|", "\n")
                                            .replace(",", "\n")
                                            .replace(" ", "\n")
                                     };

                                     let render_val = move |k: &str| {
                                         let s = get_str(k);
                                         s.split('\n')
                                            .filter(|line| !line.trim().is_empty())
                                            .map(|line| view! { <div>{line.to_string()}</div> })
                                            .collect_view()
                                     };

                                     let ts = hb.timestamp_us;
                                     view! {
                                        <div class="sys-card">
                                            <div class="sys-card-header">
                                                {move || view! { <span class="flash-update" key=ts></span> }}
                                                <span>{format!("SYS {} / COMP {}", key.0, key.1)}</span>
                                            </div>
                                            <div class="sys-info-row"><span class="sys-label">Type:</span> <div class="sys-val">{render_val("type")}</div></div>
                                            <div class="sys-info-row"><span class="sys-label">Mode:</span> <div class="sys-val">{render_val("base_mode")}</div></div>
                                            <div class="sys-info-row"><span class="sys-label">Status:</span> <div class="sys-val">{render_val("system_status")}</div></div>
                                        </div>
                                     }.into_view()
                                 },
                                 None => view! { <span style="display:none"></span> }.into_view()
                             }}
                        }
                    }
                />
            </div>

            // 2. Main Registry Table
            <div style="flex: 1; overflow-y: auto; overflow-x: auto;">
                <table class="msg-table">
                    <thead>
                        <tr style="border-bottom: 2px solid #444; color: #888; text-align: left;">
                            <th style="min-width: 40px; padding: 5px;">#</th>
                            <th style="min-width: 150px; padding: 5px;">Message</th>
                            <th style="min-width: 60px; padding: 5px; text-align: center;">Seq</th>
                            <th style="min-width: 120px; text-align: right; padding: 5px;">Timestamp</th>
                            <th style="padding: 5px; width: 100%;">Data</th>
                        </tr>
                    </thead>
                    <tbody>
                        <For
                            each=move || system_keys.get()
                            key=|k| *k
                            children=move |key| {
                                // Reactive slice for messages
                                let messages = create_memo(move |_| {
                                    systems_map.with(|m| {
                                        m.get(&key).map(|(_, msgs)| msgs.values().cloned().collect::<Vec<_>>()).unwrap_or_default()
                                    })
                                });

                                view! {
                                    // Group Header
                                    <tr class="group-header">
                                        <td colspan="5" style="padding: 8px; background: #222; color: #bbb;">
                                            {format!("System {} / Component {}", key.0, key.1)}
                                        </td>
                                    </tr>
                                    // Rows
                                    <For
                                        each=move || messages.get()
                                        // Key MUST include timestamp to force re-render/flash on update
                                        key=|item| format!("{}-{}", item.summary, item.timestamp_us)
                                        children=move |event| {

                                            // Extract Seq
                                            let seq_str = event.fields.iter()
                                                .find(|(k, _)| k == "seq")
                                                .map(|(_, v)| v.to_string())
                                                .unwrap_or_default();

                                            // Summary without meta fields
                                            let summary = event.fields.iter()
                                                .filter(|(k, _)| k != "sys_id" && k != "comp_id" && k != "seq" && k != "version")
                                                .take(5)
                                                .map(|(k, v)| format!("{}:{}", k, v))
                                                .collect::<Vec<_>>()
                                                .join("  ");

                                            let ts = event.timestamp_us;

                                            view! {
                                                <tr class="msg-row">
                                                    <td class="msg-cell">
                                                        {move || view! { <span class="flash-update" key=ts></span> }}
                                                    </td>
                                                    <td class="msg-cell" style="color: #4CAF50; font-weight: bold;">
                                                        {event.summary.to_string()}
                                                    </td>
                                                    <td class="msg-cell" style="color: #aaa; text-align: center;">
                                                        {seq_str}
                                                    </td>
                                                    <td class="msg-cell" style="text-align: right; color: #888;">
                                                        {event.timestamp_us}
                                                    </td>
                                                    <td class="msg-cell" style="color: #aaa; word-break: break-all;">
                                                        {summary}
                                                    </td>
                                                </tr>
                                            }
                                        }
                                    />
                                }
                            }
                        />
                    </tbody>
                </table>
            </div>
        </div>
    }
}
