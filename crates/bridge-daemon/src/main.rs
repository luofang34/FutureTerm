mod protocol;
mod serial;
mod server;

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

    // Single instance check: if port is already bound, another instance is running
    let listener = match TcpListener::bind(format!("127.0.0.1:{}", port)).await {
        Ok(l) => l,
        Err(_) => {
            eprintln!("Port {} already in use (daemon already running)", port);
            std::process::exit(0);
        }
    };

    // Start WebSocket server with auto-shutdown (5 minutes idle)
    server::serve(listener, Duration::from_secs(300)).await?;

    Ok(())
}
