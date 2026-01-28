/// Calculates the retry delay in milliseconds for a given attempt number.
///
/// Uses exponential backoff:
/// - Base delay: 100ms
/// - Multiplier: 2^(attempt - 1)
/// - Jitter: Random 0-50ms (simulated or injected)
///
/// # Arguments
/// * `attempt` - The current retry attempt number (1-based)
///
/// # Returns
/// Delay in milliseconds
pub fn calculate_retry_delay(attempt: u32) -> u64 {
    if attempt == 0 {
        return 0;
    }
    // Cap at reasonable max (e.g. 10 attempts -> ~50 seconds is too much?
    // Existing logic was 1..=10.
    // 100 * 2^0 = 100
    // 100 * 2^9 = 51200ms -> 51s. This matches existing logic.

    // Safety: limited attempt count prevents overflow generally, but saturating for safety
    let attempt_idx = attempt.saturating_sub(1);
    let shift = attempt_idx.min(30); // Prevent overflow of u64 shift

    let base_delay = 100u64.saturating_mul(1 << shift);

    // Add simple pseudo-random jitter if on native, or full random on WASM
    // For pure unit testing without randomness, we could split this.
    // But keeping it simple: just return base + jitter.

    #[cfg(target_arch = "wasm32")]
    let jitter = (js_sys::Math::random() * 50.0) as u64;

    #[cfg(not(target_arch = "wasm32"))]
    let jitter = 0; // Deterministic for native tests usually desirable, or use rand if imported

    base_delay.saturating_add(jitter)
}

#[cfg(test)]
#[allow(clippy::panic, clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn test_backoff_calculation() {
        // Attempt 1: 100ms * 2^0 = 100
        assert_eq!(calculate_retry_delay(1), 100);

        // Attempt 2: 100ms * 2^1 = 200
        assert_eq!(calculate_retry_delay(2), 200);

        // Attempt 3: 100ms * 2^2 = 400
        assert_eq!(calculate_retry_delay(3), 400);

        // Attempt 10: 100ms * 2^9 = 51200
        assert_eq!(calculate_retry_delay(10), 51200);
    }

    #[test]
    fn test_safety_overflow() {
        // Should not panic on high numbers
        let delay = calculate_retry_delay(100);
        assert!(delay > 0);
    }
}
