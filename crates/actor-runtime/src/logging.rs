/// Centralized logging macros for actor system
///
/// These macros provide consistent logging across all actors with:
/// - Platform-specific output (web_sys::console on WASM, eprintln! on native)
/// - Debug-only compilation (stripped from release builds)
/// - Consistent formatting with actor context
///
/// Log debug-level message (only in debug builds)
///
/// # Example
/// ```
/// use actor_runtime::actor_debug;
/// actor_debug!("StateActor: {:?} → {:?}", "Disconnected", "Connected");
/// ```
#[macro_export]
macro_rules! actor_debug {
    ($($arg:tt)*) => {
        #[cfg(debug_assertions)]
        {
            #[cfg(target_arch = "wasm32")]
            web_sys::console::log_1(&format!($($arg)*).into());
            #[cfg(not(target_arch = "wasm32"))]
            eprintln!("[DEBUG] {}", format!($($arg)*));
        }
    };
}

/// Log info-level message (only in debug builds)
///
/// Use for important state changes and user-facing events
#[macro_export]
macro_rules! actor_info {
    ($($arg:tt)*) => {
        #[cfg(debug_assertions)]
        {
            #[cfg(target_arch = "wasm32")]
            web_sys::console::info_1(&format!($($arg)*).into());
            #[cfg(not(target_arch = "wasm32"))]
            eprintln!("[INFO] {}", format!($($arg)*));
        }
    };
}

/// Log warning-level message (only in debug builds)
///
/// Use for recoverable errors and unexpected conditions
#[macro_export]
macro_rules! actor_warn {
    ($($arg:tt)*) => {
        #[cfg(debug_assertions)]
        {
            #[cfg(target_arch = "wasm32")]
            web_sys::console::warn_1(&format!($($arg)*).into());
            #[cfg(not(target_arch = "wasm32"))]
            eprintln!("[WARN] {}", format!($($arg)*));
        }
    };
}

/// Log error-level message (always compiled, even in release)
///
/// Use for critical errors that should always be visible
#[macro_export]
macro_rules! actor_error {
    ($($arg:tt)*) => {
        {
            #[cfg(target_arch = "wasm32")]
            web_sys::console::error_1(&format!($($arg)*).into());
            #[cfg(not(target_arch = "wasm32"))]
            eprintln!("[ERROR] {}", format!($($arg)*));
        }
    };
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    #[test]
    fn test_logging_macros_compile() {
        // Just verify macros compile
        actor_debug!("test debug");
        actor_info!("test info");
        actor_warn!("test warn");
        actor_error!("test error");
    }

    #[test]
    fn test_logging_with_format_args() {
        actor_debug!("StateActor: {} → {}", "Connected", "Disconnected");
        actor_info!("Port opened at {} baud", 115200);
        actor_warn!("Retry attempt {}/{}", 1, 5);
        actor_error!("Failed to open port: {}", "Access denied");
    }
}
