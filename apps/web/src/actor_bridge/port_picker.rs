use wasm_bindgen::JsCast;

impl super::ActorBridge {
    /// Request port from user (shows browser port picker)
    pub async fn request_port(&self) -> Option<web_sys::SerialPort> {
        let window = web_sys::window()?;
        let navigator = window.navigator();
        let serial = navigator.serial();

        let promise = serial.request_port();

        match wasm_bindgen_futures::JsFuture::from(promise).await {
            Ok(port_js) => port_js.dyn_into::<web_sys::SerialPort>().ok(),
            Err(_) => None,
        }
    }

    /// Auto-select port by VID/PID (skip picker if match found)
    /// If VID/PID is None (fresh session), auto-selects if only 1 device available
    pub async fn auto_select_port(
        &self,
        vid: Option<u16>,
        pid: Option<u16>,
    ) -> Option<web_sys::SerialPort> {
        #[cfg(debug_assertions)]
        web_sys::console::log_1(
            &format!(
                "DEBUG: auto_select_port called with VID={:04X?}, PID={:04X?}",
                vid, pid
            )
            .into(),
        );

        let window = web_sys::window()?;
        let navigator = window.navigator();
        let serial = navigator.serial();

        let ports_promise = serial.get_ports();
        let ports_js = wasm_bindgen_futures::JsFuture::from(ports_promise)
            .await
            .ok()?;
        let ports_array: js_sys::Array = ports_js.dyn_into().ok()?;

        #[cfg(debug_assertions)]
        web_sys::console::log_1(
            &format!("DEBUG: getPorts() returned {} ports", ports_array.length()).into(),
        );

        let mut exact_match: Option<web_sys::SerialPort> = None;
        let mut fallback_port: Option<web_sys::SerialPort> = None;
        let mut port_count = 0;

        for i in 0..ports_array.length() {
            let port_js = ports_array.get(i);
            if let Ok(port) = port_js.dyn_into::<web_sys::SerialPort>() {
                let info = port.get_info();
                let port_vid = js_sys::Reflect::get(&info, &"usbVendorId".into())
                    .ok()
                    .and_then(|v| v.as_f64())
                    .map(|v| v as u16);
                let port_pid = js_sys::Reflect::get(&info, &"usbProductId".into())
                    .ok()
                    .and_then(|v| v.as_f64())
                    .map(|v| v as u16);

                #[cfg(debug_assertions)]
                web_sys::console::log_1(
                    &format!(
                        "DEBUG: Port {}: VID={:04X?}, PID={:04X?}",
                        i, port_vid, port_pid
                    )
                    .into(),
                );

                // Check for exact match (if VID/PID was provided)
                if let (Some(target_vid), Some(target_pid)) = (vid, pid) {
                    if port_vid == Some(target_vid) && port_pid == Some(target_pid) {
                        #[cfg(debug_assertions)]
                        web_sys::console::log_1(&"DEBUG: Exact match found!".into());
                        exact_match = Some(port);
                        break;
                    }
                }

                // Count real USB devices (ignore virtual COM ports)
                if port_vid.is_some() && port_pid.is_some() {
                    port_count += 1;
                    fallback_port = Some(port);
                }
            }
        }

        // Return exact match if found
        if let Some(port) = exact_match {
            return Some(port);
        }

        // Fallback: If only 1 real USB device available, use it
        // (Handles fresh session or device swap)
        if port_count == 1 {
            #[cfg(debug_assertions)]
            if vid.is_none() {
                web_sys::console::log_1(
                    &"DEBUG: Fresh session (no stored device), auto-selecting single USB device"
                        .into(),
                );
            } else {
                web_sys::console::log_1(
                    &"DEBUG: No exact match, but only 1 USB device available. Using it as fallback"
                        .into(),
                );
            }

            if fallback_port.is_some() {
                #[cfg(debug_assertions)]
                web_sys::console::log_1(&"DEBUG: Returning fallback port".into());
                return fallback_port;
            } else {
                #[cfg(debug_assertions)]
                web_sys::console::log_1(&"DEBUG: ERROR - fallback_port is None!".into());
            }
        }

        #[cfg(debug_assertions)]
        web_sys::console::log_1(
            &format!(
                "DEBUG: {} USB devices available, showing picker (fallback_port={:?})",
                port_count,
                fallback_port.is_some()
            )
            .into(),
        );

        None
    }
}
