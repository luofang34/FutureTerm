//! Cancellation utilities for interruptible actor operations
//!
//! Provides helpers for racing futures against cancellation flags,
//! enabling responsive operation interruption.

#[cfg(target_arch = "wasm32")]
use std::future::Future;
#[cfg(target_arch = "wasm32")]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(target_arch = "wasm32")]
use std::sync::Arc;

/// Default poll interval for cancellation checks (50ms)
#[cfg(target_arch = "wasm32")]
pub const DEFAULT_CANCEL_POLL_MS: u64 = 50;

/// Creates a future that completes when the interrupt flag is set
///
/// Polls the flag every 50ms by default for responsive cancellation.
#[cfg(target_arch = "wasm32")]
pub async fn create_cancel_future(flag: Arc<AtomicBool>) {
    create_cancel_future_with_interval(flag, DEFAULT_CANCEL_POLL_MS).await
}

/// Creates a cancel future with custom poll interval
#[cfg(target_arch = "wasm32")]
pub async fn create_cancel_future_with_interval(flag: Arc<AtomicBool>, poll_interval_ms: u64) {
    loop {
        if flag.load(Ordering::Acquire) {
            break;
        }
        gloo_timers::future::sleep(std::time::Duration::from_millis(poll_interval_ms)).await;
    }
}

/// Races a future against cancellation, returns None if cancelled
///
/// # Example
/// ```ignore
/// let result = race_with_cancellation(
///     port.open(),
///     interrupt_flag.clone(),
/// ).await;
///
/// match result {
///     Some(Ok(data)) => println!("Got data"),
///     Some(Err(e)) => println!("Error: {}", e),
///     None => println!("Cancelled by user"),
/// }
/// ```
#[cfg(target_arch = "wasm32")]
pub async fn race_with_cancellation<T, F>(fut: F, cancel_flag: Arc<AtomicBool>) -> Option<T>
where
    F: Future<Output = T>,
{
    use futures::future::{select, Either};

    let cancel_fut = create_cancel_future(cancel_flag);

    match select(Box::pin(fut), Box::pin(cancel_fut)).await {
        Either::Left((result, _)) => Some(result),
        Either::Right(_) => None,
    }
}

#[cfg(all(test, target_arch = "wasm32"))]
mod tests {
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use wasm_bindgen_test::*;

    wasm_bindgen_test_configure!(run_in_browser);

    #[wasm_bindgen_test]
    async fn test_cancel_future_completes_when_flag_set() {
        let flag = Arc::new(AtomicBool::new(false));
        let flag_clone = flag.clone();

        // Spawn async task
        let handle = wasm_bindgen_futures::spawn_local(async move {
            super::create_cancel_future_with_interval(flag_clone, 10).await;
        });

        gloo_timers::future::sleep(std::time::Duration::from_millis(50)).await;
        flag.store(true, Ordering::Release);

        // Wait a bit to let the cancel future complete
        gloo_timers::future::sleep(std::time::Duration::from_millis(20)).await;
    }

    #[wasm_bindgen_test]
    async fn test_race_returns_none_when_cancelled() {
        let flag = Arc::new(AtomicBool::new(false));

        // Set flag immediately so race cancels
        flag.store(true, Ordering::Release);

        let result = super::race_with_cancellation(
            async { gloo_timers::future::sleep(std::time::Duration::from_secs(10)).await },
            flag,
        )
        .await;

        assert!(result.is_none());
    }
}
