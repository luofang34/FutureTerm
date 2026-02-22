mod protocol;
mod serial;
mod server;
mod tls;

use std::time::Duration;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Parse port from argv (default 9876).
    // Reject privileged ports (< 1024) to avoid permission errors and surprises.
    let port: u16 = std::env::args()
        .nth(1)
        .and_then(|s| {
            let p: u16 = s.parse().ok()?;
            if p >= 1024 {
                Some(p)
            } else {
                None
            }
        })
        .unwrap_or(9876);

    // Install crypto provider (both ring and aws-lc-rs are pulled in by deps;
    // rustls needs exactly one chosen explicitly).
    let _ = rustls::crypto::ring::default_provider().install_default();

    eprintln!("FutureTerm Bridge Daemon v{}", env!("CARGO_PKG_VERSION"));

    // Fetch ACME certificate for local.futureterm.app from the cert worker.
    // If unavailable, daemon runs without TLS — Chrome/Edge still use WebSerial
    // directly, but Safari/Firefox bridge will be disabled.
    let tls_acceptor = tls::load_tls_acceptor().await;

    if tls_acceptor.is_some() {
        eprintln!("TLS enabled: wss://local.futureterm.app:{}", port);
    } else {
        eprintln!(
            "TLS unavailable: ws://127.0.0.1:{} (Safari/Firefox bridge disabled)",
            port
        );
    }

    // Single instance check: if port is already bound, another instance is running
    let listener = match TcpListener::bind(format!("127.0.0.1:{}", port)).await {
        Ok(l) => l,
        Err(_) => {
            eprintln!("Port {} already in use (daemon already running)", port);
            std::process::exit(0);
        }
    };

    // Start WebSocket server with auto-shutdown (2 minutes idle)
    server::serve(listener, Duration::from_secs(120), tls_acceptor).await?;

    Ok(())
}
