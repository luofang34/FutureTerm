mod protocol;
mod serial;
mod server;
mod tls;

use std::time::Duration;
use tokio::net::TcpListener;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Parse port from argv (default 9876)
    let port = std::env::args()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .unwrap_or(9876);

    eprintln!("FutureTerm Bridge Daemon v{}", env!("CARGO_PKG_VERSION"));
    eprintln!("Attempting to bind to 127.0.0.1:{}", port);

    // Single instance check (prevent abuse)
    // If port is already bound, another instance is running
    let listener = match TcpListener::bind(format!("127.0.0.1:{}", port)).await {
        Ok(l) => {
            eprintln!("Successfully bound to port {}", port);
            l
        }
        Err(e) => {
            eprintln!("Port {} already in use (daemon already running)", port);
            eprintln!("Error: {}", e);
            std::process::exit(0); // Silent exit
        }
    };

    // Generate TLS cert if needed (first run only)
    let (cert, key) = match tls::ensure_tls_cert() {
        Ok((c, k)) => (c, k),
        Err(e) => {
            eprintln!("Failed to ensure TLS certificate: {}", e);
            eprintln!("Note: TLS is currently not fully implemented in this version");
            eprintln!("Using plain WebSocket for now");
            (vec![], vec![])
        }
    };

    // Start WebSocket server with auto-shutdown (5 minutes)
    eprintln!("Starting WebSocket server with 5-minute idle timeout");
    server::serve(listener, cert, key, Duration::from_secs(300)).await?;

    Ok(())
}
