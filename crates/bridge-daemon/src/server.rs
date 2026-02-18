use crate::protocol::{ClientMessage, ServerMessage};
use crate::serial::SerialManager;
use futures_util::{SinkExt, StreamExt};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, RwLock};
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

const ALLOWED_ORIGINS: &[&str] = &[
    "https://futureterm.com",
    // "https://futureterm.app",  // User owns this domain, enable when ready
    "http://localhost:8080",
    "http://127.0.0.1:8080",
];

#[allow(dead_code)] // Used in tests
const IDLE_TIMEOUT: Duration = Duration::from_secs(300); // 5 minutes
const IDLE_CHECK_INTERVAL: Duration = Duration::from_secs(60); // Check every 60 seconds

/// WebSocket server with Origin validation and auto-shutdown
pub async fn serve(listener: TcpListener, idle_timeout: Duration) -> Result<(), String> {
    eprintln!(
        "WebSocket server listening on {}",
        listener.local_addr().map_err(|e| e.to_string())?
    );

    let last_activity = Arc::new(RwLock::new(Instant::now()));
    let last_activity_clone = last_activity.clone();

    // Spawn idle timeout checker
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(IDLE_CHECK_INTERVAL).await;
            let elapsed = last_activity_clone.read().await.elapsed();
            if elapsed > idle_timeout {
                eprintln!("Idle timeout ({:?}), shutting down", idle_timeout);
                std::process::exit(0);
            }
        }
    });

    loop {
        let (stream, addr) = listener
            .accept()
            .await
            .map_err(|e| format!("Failed to accept connection: {}", e))?;

        eprintln!("New connection from {}", addr);

        let last_activity = last_activity.clone();
        tokio::spawn(async move {
            if let Err(e) = handle_connection(stream, last_activity).await {
                eprintln!("Connection error: {}", e);
            }
        });
    }
}

/// Handle a single WebSocket connection
async fn handle_connection(
    stream: TcpStream,
    last_activity: Arc<RwLock<Instant>>,
) -> Result<(), String> {
    // Validate Origin header during WebSocket handshake
    let callback =
        |req: &tokio_tungstenite::tungstenite::handshake::server::Request,
         response: tokio_tungstenite::tungstenite::handshake::server::Response| {
            let origin = req.headers().get("Origin").and_then(|h| h.to_str().ok());

            match origin {
                Some(o) if ALLOWED_ORIGINS.contains(&o) => {
                    eprintln!("Accepted connection from origin: {}", o);
                    Ok(response)
                }
                Some(o) => {
                    eprintln!("Rejected connection from unauthorized origin: {}", o);
                    Err(
                        tokio_tungstenite::tungstenite::handshake::server::ErrorResponse::new(
                            Some("Unauthorized origin".into()),
                        ),
                    )
                }
                None => {
                    eprintln!("Rejected connection with missing Origin header");
                    Err(
                        tokio_tungstenite::tungstenite::handshake::server::ErrorResponse::new(
                            Some("Missing Origin header".into()),
                        ),
                    )
                }
            }
        };

    let ws_stream = tokio_tungstenite::accept_hdr_async(stream, callback)
        .await
        .map_err(|e| format!("WebSocket handshake failed: {}", e))?;

    handle_websocket(ws_stream, last_activity).await
}

/// Handle WebSocket messages
async fn handle_websocket(
    ws_stream: WebSocketStream<TcpStream>,
    last_activity: Arc<RwLock<Instant>>,
) -> Result<(), String> {
    let (mut ws_sender, mut ws_receiver) = ws_stream.split();
    let mut serial_manager = SerialManager::new();

    // Channel for serial data
    let (data_tx, mut data_rx) = mpsc::unbounded_channel::<Vec<u8>>();

    // Channel for outgoing WebSocket messages
    let (ws_tx, mut ws_rx) = mpsc::unbounded_channel::<String>();

    // Spawn task to forward messages to WebSocket
    let last_activity_clone = last_activity.clone();
    tokio::spawn(async move {
        loop {
            tokio::select! {
                Some(data) = data_rx.recv() => {
                    // Update activity timestamp
                    *last_activity_clone.write().await = Instant::now();

                    // Encode as base64
                    let encoded = base64_encode(&data);
                    let msg = ServerMessage::Data { data: encoded };

                    if let Ok(json) = msg.to_json() {
                        if ws_sender.send(Message::Text(json.into())).await.is_err() {
                            break;
                        }
                    }
                }
                Some(json) = ws_rx.recv() => {
                    if ws_sender.send(Message::Text(json.into())).await.is_err() {
                        break;
                    }
                }
                else => break,
            }
        }
    });

    // Process incoming WebSocket messages
    while let Some(msg) = ws_receiver.next().await {
        // Update activity timestamp
        *last_activity.write().await = Instant::now();

        let msg = msg.map_err(|e| format!("WebSocket error: {}", e))?;

        match msg {
            Message::Text(text) => {
                let (response, disconnect_rx) =
                    handle_client_message(&text, &mut serial_manager, data_tx.clone()).await;
                let json = response.to_json()?;
                ws_tx
                    .send(json)
                    .map_err(|e| format!("Failed to send response: {}", e))?;

                // If this was an Open command, watch for serial port disconnect
                if let Some(rx) = disconnect_rx {
                    let ws_tx_disc = ws_tx.clone();
                    tokio::spawn(async move {
                        if let Ok(reason) = rx.await {
                            let msg = ServerMessage::PortDisconnected { reason };
                            if let Ok(json) = msg.to_json() {
                                let _ = ws_tx_disc.send(json);
                            }
                        }
                    });
                }
            }
            Message::Close(_) => {
                eprintln!("Client closed connection");
                break;
            }
            _ => {}
        }
    }

    // Clean up
    serial_manager.close().await;
    Ok(())
}

/// Handle a single client message.
///
/// Returns the response and optionally a disconnect receiver (for Open commands)
/// that fires when the serial port disconnects.
async fn handle_client_message(
    text: &str,
    serial_manager: &mut SerialManager,
    data_tx: mpsc::UnboundedSender<Vec<u8>>,
) -> (
    ServerMessage,
    Option<tokio::sync::oneshot::Receiver<String>>,
) {
    let msg = match ClientMessage::from_json(text) {
        Ok(m) => m,
        Err(e) => return (ServerMessage::error(None, e), None),
    };

    let _id = msg.id();

    match msg {
        ClientMessage::ListPorts { id } => match SerialManager::list_ports() {
            Ok(ports) => (ServerMessage::PortsList { id, ports }, None),
            Err(e) => (ServerMessage::error(Some(id), e), None),
        },
        ClientMessage::Open {
            id,
            path,
            baud_rate,
        } => {
            let (disconnect_tx, disconnect_rx) = tokio::sync::oneshot::channel();
            match serial_manager
                .open(&path, baud_rate, data_tx, Some(disconnect_tx))
                .await
            {
                Ok(()) => (ServerMessage::Opened { id }, Some(disconnect_rx)),
                Err(e) => (ServerMessage::error(Some(id), e), None),
            }
        }
        ClientMessage::Close { id } => {
            serial_manager.close().await;
            (ServerMessage::Closed { id }, None)
        }
        ClientMessage::Write { id, data } => {
            let decoded = match base64_decode(&data) {
                Ok(d) => d,
                Err(e) => return (ServerMessage::error(Some(id), e), None),
            };

            eprintln!("Serial TX: {} bytes", decoded.len());
            match serial_manager.write(&decoded).await {
                Ok(bytes) => (ServerMessage::Written { id, bytes }, None),
                Err(e) => {
                    eprintln!("Serial TX error: {}", e);
                    (ServerMessage::error(Some(id), e), None)
                }
            }
        }
        ClientMessage::SetConfig {
            id,
            baud_rate,
            data_bits,
            stop_bits,
            parity,
        } => match serial_manager
            .set_config(baud_rate, data_bits, stop_bits, parity)
            .await
        {
            Ok(()) => (ServerMessage::ConfigSet { id }, None),
            Err(e) => (ServerMessage::error(Some(id), e), None),
        },
    }
}

/// Simple base64 encoding
fn base64_encode(data: &[u8]) -> String {
    use std::io::Write;
    let mut buf = Vec::new();
    {
        let mut encoder =
            base64::write::EncoderWriter::new(&mut buf, &base64::engine::general_purpose::STANDARD);
        encoder.write_all(data).unwrap_or_default();
    }
    String::from_utf8(buf).unwrap_or_default()
}

/// Simple base64 decoding
fn base64_decode(data: &str) -> Result<Vec<u8>, String> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD
        .decode(data)
        .map_err(|e| format!("Base64 decode error: {}", e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_base64_encode() {
        assert_eq!(base64_encode(b"Hello"), "SGVsbG8=");
        assert_eq!(base64_encode(b""), "");
        assert_eq!(base64_encode(b"test"), "dGVzdA==");
    }

    #[test]
    fn test_base64_decode() {
        assert_eq!(base64_decode("SGVsbG8=").unwrap(), b"Hello");
        assert_eq!(base64_decode("").unwrap(), b"");
        assert_eq!(base64_decode("dGVzdA==").unwrap(), b"test");
    }

    #[test]
    fn test_base64_roundtrip() {
        let data = b"Hello, World!";
        let encoded = base64_encode(data);
        let decoded = base64_decode(&encoded).unwrap();
        assert_eq!(decoded, data);
    }

    #[test]
    fn test_allowed_origins() {
        assert!(ALLOWED_ORIGINS.contains(&"https://futureterm.com"));
        assert!(ALLOWED_ORIGINS.contains(&"http://localhost:8080"));
        assert!(ALLOWED_ORIGINS.contains(&"http://127.0.0.1:8080"));
        assert!(!ALLOWED_ORIGINS.contains(&"https://evil.com"));
    }
}
