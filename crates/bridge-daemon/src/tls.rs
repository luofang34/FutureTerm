use rustls::pki_types::CertificateDer;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_rustls::TlsAcceptor;

/// Cert storage directory: ~/Library/Application Support/FutureTerm/ (macOS)
/// or ~/.local/share/FutureTerm/ (Linux).
fn cert_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("FutureTerm"))
}

fn cert_path() -> Option<PathBuf> {
    cert_dir().map(|d| d.join("local-cert.pem"))
}

fn key_path() -> Option<PathBuf> {
    cert_dir().map(|d| d.join("local-key.pem"))
}

/// Load or generate a self-signed TLS certificate for 127.0.0.1.
///
/// First run: generates a self-signed cert, saves to disk, and adds to the
/// macOS Keychain as trusted for SSL (prompts user for authentication once).
///
/// Subsequent runs: loads the cached cert from disk.
pub async fn load_tls_acceptor() -> Option<TlsAcceptor> {
    // Try loading existing cert from disk
    if let Some((cert_pem, key_pem)) = load_cached_cert().await {
        if let Ok(acceptor) = build_acceptor(&cert_pem, &key_pem) {
            eprintln!("TLS: Using existing local certificate");
            return Some(acceptor);
        }
        eprintln!("TLS: Cached certificate is invalid, regenerating...");
    }

    // Generate new self-signed certificate
    let (cert_pem, key_pem) = match generate_self_signed_cert() {
        Ok(pair) => pair,
        Err(e) => {
            eprintln!("TLS: Failed to generate certificate: {}", e);
            return None;
        }
    };
    eprintln!("TLS: Generated new self-signed certificate for 127.0.0.1");

    // Save to disk
    if let Err(e) = save_cert_to_disk(&cert_pem, &key_pem).await {
        eprintln!("TLS: Failed to save certificate: {}", e);
    }

    // Add to macOS Keychain so browsers trust wss://127.0.0.1
    #[cfg(target_os = "macos")]
    if let Some(cert_p) = cert_path() {
        match add_to_macos_keychain(&cert_p) {
            Ok(()) => eprintln!("TLS: Certificate trusted in macOS Keychain"),
            Err(e) => eprintln!(
                "TLS: Keychain trust failed: {} (Safari may reject wss://127.0.0.1)",
                e
            ),
        }
    }

    build_acceptor(&cert_pem, &key_pem).ok()
}

/// Generate a self-signed certificate valid for 127.0.0.1 and localhost.
/// Uses ECDSA P-256 for compact, fast certs. Valid for ~11 years (rcgen default).
fn generate_self_signed_cert() -> Result<(String, String), String> {
    use rcgen::{CertificateParams, KeyPair, SanType};
    use std::net::{IpAddr, Ipv4Addr};

    let mut params = CertificateParams::new(vec!["localhost".into()])
        .map_err(|e| format!("Cert params: {}", e))?;

    // Add IP SAN for 127.0.0.1 (browsers check SAN, not CN)
    params
        .subject_alt_names
        .push(SanType::IpAddress(IpAddr::V4(Ipv4Addr::LOCALHOST)));

    let key_pair = KeyPair::generate().map_err(|e| format!("Key generation: {}", e))?;

    let cert = params
        .self_signed(&key_pair)
        .map_err(|e| format!("Self-signing: {}", e))?;

    Ok((cert.pem(), key_pair.serialize_pem()))
}

/// Add a certificate to the macOS Keychain as trusted for SSL.
/// On first run, macOS shows an authentication dialog (user enters password once).
///
/// Uses `spawn()` so the daemon starts serving immediately while the user
/// interacts with the password dialog. Chrome works instantly (exempts localhost);
/// Safari starts working once the user enters their password.
#[cfg(target_os = "macos")]
fn add_to_macos_keychain(cert_path: &std::path::Path) -> Result<(), String> {
    let _child = std::process::Command::new("security")
        .args(["add-trusted-cert", "-p", "ssl"])
        .arg(cert_path)
        .spawn()
        .map_err(|e| format!("Failed to run security: {}", e))?;

    // Child process runs in background — macOS SecurityAgent will show
    // the authentication dialog. We don't wait for it to complete.
    Ok(())
}

fn build_acceptor(cert_pem: &str, key_pem: &str) -> Result<TlsAcceptor, String> {
    let certs: Vec<CertificateDer<'static>> = rustls_pemfile::certs(&mut cert_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|e| format!("Failed to parse certs: {}", e))?;

    if certs.is_empty() {
        return Err("No certificates found in PEM".into());
    }

    let key = rustls_pemfile::private_key(&mut key_pem.as_bytes())
        .map_err(|e| format!("Failed to parse key: {}", e))?
        .ok_or("No private key found in PEM")?;

    let config = rustls::ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .map_err(|e| format!("TLS config error: {}", e))?;

    Ok(TlsAcceptor::from(Arc::new(config)))
}

/// Load cached cert+key from disk.
async fn load_cached_cert() -> Option<(String, String)> {
    let cert_p = cert_path()?;
    let key_p = key_path()?;

    let cert_pem = tokio::fs::read_to_string(&cert_p).await.ok()?;
    let key_pem = tokio::fs::read_to_string(&key_p).await.ok()?;

    if cert_pem.is_empty() || key_pem.is_empty() {
        return None;
    }

    Some((cert_pem, key_pem))
}

async fn save_cert_to_disk(cert_pem: &str, key_pem: &str) -> Result<(), String> {
    let dir = cert_dir().ok_or("Cannot determine cert directory")?;
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("Failed to create cert dir: {}", e))?;

    let cert_p = dir.join("local-cert.pem");
    let key_p = dir.join("local-key.pem");

    // Write certificate normally (world readable is fine for public certs)
    tokio::fs::write(&cert_p, cert_pem)
        .await
        .map_err(|e| format!("Failed to write cert: {}", e))?;

    // Create a strict `0o600` options handle to restrict private key access
    let mut key_opts = tokio::fs::OpenOptions::new();
    key_opts.write(true).create(true).truncate(true);

    #[cfg(unix)]
    key_opts.mode(0o600);

    let mut key_file = key_opts
        .open(&key_p)
        .await
        .map_err(|e| format!("Failed to open key file: {}", e))?;

    use tokio::io::AsyncWriteExt;
    key_file
        .write_all(key_pem.as_bytes())
        .await
        .map_err(|e| format!("Failed to write key file: {}", e))?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_generate_self_signed_cert() {
        let (cert_pem, key_pem) = generate_self_signed_cert().expect("cert generation failed");
        assert!(cert_pem.contains("BEGIN CERTIFICATE"));
        assert!(key_pem.contains("BEGIN PRIVATE KEY"));
    }

    #[test]
    fn test_build_acceptor_with_generated_cert() {
        // Install crypto provider (same as main.rs does at startup)
        let _ = rustls::crypto::ring::default_provider().install_default();
        let (cert_pem, key_pem) = generate_self_signed_cert().expect("cert generation failed");
        let acceptor = build_acceptor(&cert_pem, &key_pem);
        assert!(acceptor.is_ok());
    }

    #[test]
    fn test_build_acceptor_empty_cert() {
        let result = build_acceptor("", "");
        assert!(result.is_err());
    }

    #[test]
    fn test_build_acceptor_invalid_pem() {
        let result = build_acceptor("not a cert", "not a key");
        assert!(result.is_err());
    }

    #[test]
    fn test_cert_dir_exists() {
        assert!(cert_dir().is_some());
    }
}
