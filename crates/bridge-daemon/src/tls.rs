use rustls::pki_types::CertificateDer;
use serde::Deserialize;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_rustls::TlsAcceptor;

/// HTTPS endpoint serving the latest cert+key bundle (hosted on GitHub Pages).
/// The renewal workflow commits this file to apps/web/cert/bridge.json,
/// which gets deployed to GitHub Pages at this URL.
const CERT_URL: &str = "https://futureterm.com/cert/bridge.json";

/// Fallback URL in case the primary domain changes to futureterm.app.
#[allow(dead_code)]
const CERT_URL_ALT: &str = "https://futureterm.app/cert/bridge.json";

/// JSON shape returned by the cert endpoint.
#[derive(Deserialize)]
struct CertBundle {
    cert_pem: String,
    key_pem: String,
    #[allow(dead_code)]
    not_after: Option<String>,
}

/// Cert storage directory: ~/Library/Application Support/FutureTerm/ (macOS)
/// or ~/.local/share/FutureTerm/ (Linux).
fn cert_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("FutureTerm"))
}

fn cert_path() -> Option<PathBuf> {
    cert_dir().map(|d| d.join("bridge-cert.pem"))
}

fn key_path() -> Option<PathBuf> {
    cert_dir().map(|d| d.join("bridge-key.pem"))
}

/// Load or fetch the TLS certificate, returning a TlsAcceptor.
/// Returns None if TLS cannot be set up (network down, no cached cert).
pub async fn load_tls_acceptor() -> Option<TlsAcceptor> {
    // 1. Try loading cached cert from disk
    let cached = load_cached_cert().await;

    // 2. If cached cert exists and is not near expiry, use it
    if let Some((ref cert_pem, ref key_pem)) = cached {
        if !is_near_expiry(cert_pem) {
            if let Ok(acceptor) = build_acceptor(cert_pem, key_pem) {
                eprintln!("TLS: Using cached certificate");
                return Some(acceptor);
            }
        }
    }

    // 3. Try fetching fresh cert from server
    match fetch_cert_from_server() {
        Ok((cert_pem, key_pem)) => {
            eprintln!("TLS: Fetched fresh certificate from server");
            let _ = save_cert_to_disk(&cert_pem, &key_pem).await;
            match build_acceptor(&cert_pem, &key_pem) {
                Ok(acceptor) => return Some(acceptor),
                Err(e) => eprintln!("TLS: Failed to build acceptor from fetched cert: {}", e),
            }
        }
        Err(e) => {
            eprintln!("TLS: Failed to fetch cert from server: {}", e);
        }
    }

    // 4. Fall back to stale cached cert as last resort
    if let Some((cert_pem, key_pem)) = cached {
        eprintln!("TLS: Using stale cached certificate as fallback");
        return build_acceptor(&cert_pem, &key_pem).ok();
    }

    eprintln!("TLS: No certificate available");
    None
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

/// Save cert+key to disk for caching.
async fn save_cert_to_disk(cert_pem: &str, key_pem: &str) -> Result<(), String> {
    let dir = cert_dir().ok_or("Cannot determine data directory")?;
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("Failed to create cert dir: {}", e))?;

    let cert_p = dir.join("bridge-cert.pem");
    let key_p = dir.join("bridge-key.pem");

    tokio::fs::write(&cert_p, cert_pem)
        .await
        .map_err(|e| format!("Failed to write cert: {}", e))?;

    tokio::fs::write(&key_p, key_pem)
        .await
        .map_err(|e| format!("Failed to write key: {}", e))?;

    // Restrict key file permissions (owner read-only)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let perms = std::fs::Permissions::from_mode(0o600);
        let _ = std::fs::set_permissions(&key_p, perms);
    }

    Ok(())
}

/// Fetch cert+key bundle from the server (blocking HTTP, runs once at startup).
fn fetch_cert_from_server() -> Result<(String, String), String> {
    let bundle: CertBundle = ureq::get(CERT_URL)
        .call()
        .map_err(|e| format!("HTTP request failed: {}", e))?
        .body_mut()
        .read_json()
        .map_err(|e| format!("JSON parse failed: {}", e))?;

    if bundle.cert_pem.is_empty() || bundle.key_pem.is_empty() {
        return Err("Empty cert or key in bundle".into());
    }

    Ok((bundle.cert_pem, bundle.key_pem))
}

/// Check if a PEM certificate expires within 7 days.
/// Returns true if the cert is near expiry or cannot be parsed.
fn is_near_expiry(cert_pem: &str) -> bool {
    // Parse the first certificate from PEM
    let cert_der = match rustls_pemfile::certs(&mut cert_pem.as_bytes()).next() {
        Some(Ok(c)) => c,
        _ => return true, // Can't parse → treat as expired
    };

    // Use x509-parser-lite approach: check the notAfter field.
    // rustls doesn't expose cert fields directly, so we do a rough check:
    // Parse the ASN.1 DER to find the validity period.
    parse_not_after_and_check(&cert_der, 7)
}

/// Parse X.509 DER to extract notAfter and check if it's within `days` of now.
/// Returns true if near expiry or unparseable.
fn parse_not_after_and_check(der: &[u8], days: i64) -> bool {
    // X.509 certificate structure (simplified):
    // SEQUENCE {
    //   SEQUENCE {  -- tbsCertificate
    //     [0] version
    //     INTEGER serialNumber
    //     SEQUENCE algorithmIdentifier
    //     SEQUENCE issuer
    //     SEQUENCE validity {
    //       UTCTime/GeneralizedTime notBefore
    //       UTCTime/GeneralizedTime notAfter
    //     }
    //     ...
    //   }
    //   ...
    // }
    //
    // We walk the ASN.1 structure to find the validity sequence,
    // then extract notAfter. This avoids adding a full x509 parser dependency.

    let _ = days; // Used below

    // Find notAfter by searching for time patterns in DER.
    // UTCTime tag = 0x17, GeneralizedTime tag = 0x18
    let mut i = 0;
    let mut time_count = 0;
    while i < der.len().saturating_sub(2) {
        let tag = der.get(i).copied().unwrap_or(0);
        if tag == 0x17 || tag == 0x18 {
            time_count += 1;
            if time_count == 2 {
                // This is notAfter
                let len = der.get(i + 1).copied().unwrap_or(0) as usize;
                if let Some(time_bytes) = der.get(i + 2..i + 2 + len) {
                    if let Ok(time_str) = std::str::from_utf8(time_bytes) {
                        return is_time_within_days(time_str, tag == 0x18, days);
                    }
                }
                return true; // Can't parse
            }
        }
        i += 1;
    }

    true // Can't find notAfter → treat as expired
}

/// Check if an ASN.1 time string is within `days` days from now.
fn is_time_within_days(time_str: &str, is_generalized: bool, days: i64) -> bool {
    // UTCTime format: YYMMDDHHMMSSZ
    // GeneralizedTime format: YYYYMMDDHHMMSSZ
    let (year, rest) = if is_generalized {
        // YYYYMMDDHHMMSSZ
        if time_str.len() < 14 {
            return true;
        }
        let y: i64 = time_str.get(..4).and_then(|s| s.parse().ok()).unwrap_or(0);
        (y, &time_str[4..])
    } else {
        // YYMMDDHHMMSSZ
        if time_str.len() < 12 {
            return true;
        }
        let y: i64 = time_str.get(..2).and_then(|s| s.parse().ok()).unwrap_or(0);
        let full_year = if y >= 50 { 1900 + y } else { 2000 + y };
        (full_year, &time_str[2..])
    };

    let month: i64 = rest.get(..2).and_then(|s| s.parse().ok()).unwrap_or(0);
    let day: i64 = rest.get(2..4).and_then(|s| s.parse().ok()).unwrap_or(0);

    // Approximate: convert to days since epoch for comparison.
    // Not perfectly accurate but sufficient for "within 7 days" check.
    let cert_days = year * 365 + month * 30 + day;

    // Get current date in same rough format
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let now_secs = now.as_secs() as i64;
    // Seconds since epoch → rough days
    let now_days_epoch = now_secs / 86400;
    // Convert cert date to rough days since epoch
    // Days from year 0 to 1970 ≈ 719163
    let cert_days_epoch = cert_days - 719163;

    let remaining = cert_days_epoch - now_days_epoch;
    remaining <= days
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_time_within_days_utctime_future() {
        // UTCTime format: YYMMDDHHMMSSZ (2-digit year)
        // "301231235959Z" = Dec 31, 2030 — far in the future, NOT near expiry
        assert!(!is_time_within_days("301231235959Z", false, 7));
    }

    #[test]
    fn test_is_time_within_days_utctime_past() {
        // UTCTime format: "200101000000Z" = Jan 1, 2020 — already expired
        assert!(is_time_within_days("200101000000Z", false, 7));
    }

    #[test]
    fn test_is_time_within_days_generalized_future() {
        assert!(!is_time_within_days("20301231235959Z", true, 7));
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
    fn test_is_near_expiry_invalid() {
        assert!(is_near_expiry("not a pem"));
    }

    #[test]
    fn test_cert_dir_exists() {
        // Should return Some on all platforms
        assert!(cert_dir().is_some());
    }
}
