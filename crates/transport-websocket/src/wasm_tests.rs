use super::*;
use wasm_bindgen_test::*;

wasm_bindgen_test_configure!(run_in_browser);

#[wasm_bindgen_test]
async fn test_websocket_creation() {
    let mut transport = WebSocketTransport::new();
    assert!(!transport.is_open());

    // Test connection to invalid URL (should fail gracefully)
    let result = transport.connect("ws://localhost:99999").await;
    assert!(result.is_err());
}

#[wasm_bindgen_test]
fn test_transport_default() {
    let transport = WebSocketTransport::default();
    assert!(!transport.is_open());
}

#[wasm_bindgen_test]
fn test_base64_roundtrip() {
    let data = b"Hello, Bridge!";
    let encoded = base64_encode(data);
    let decoded = base64_decode(&encoded).unwrap_or_default();
    assert_eq!(decoded, data);
}
