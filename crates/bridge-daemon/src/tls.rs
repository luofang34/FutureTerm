//! TLS certificate management for the FutureTerm WebSocket bridge daemon.
//!
//! # Architecture: ACME cert via GitHub Releases
//!
//! The daemon serves `wss://local.futureterm.app:9876` using a Let's Encrypt
//! certificate for `local.futureterm.app` (RSA-2048).
//! The domain resolves to `127.0.0.1` — traffic stays on the user's machine.
//!
//! ## Why not self-signed certs?
//!
//! Each browser handles self-signed localhost TLS differently:
//!
//! | Browser     | Cert store              | Self-signed behavior                    |
//! |-------------|-------------------------|-----------------------------------------|
//! | Chrome/Edge | OS, but loopback exempt | Skips TLS validation for 127.0.0.1      |
//! | Safari      | macOS Keychain          | Works after `add-trusted-cert` prompt   |
//! | Firefox     | Own NSS store           | "Potential Security Risk" scary warning  |
//!
//! No single self-signed approach works for all browsers. LE certs are trusted
//! natively by all browsers — no Keychain, no NSS, no exceptions needed.
//!
//! ## Why `local.futureterm.app`?
//!
//! Same approach as Plex (`*.plex.direct`). A real domain pointing to 127.0.0.1
//! lets us use a publicly trusted CA.
//!
//! ## Security model
//!
//! The private key is shared across all daemon instances (downloaded from a
//! public GitHub Release). This is acceptable because:
//! 1. The domain resolves to 127.0.0.1 — traffic never leaves the machine.
//! 2. Exploiting a leaked key requires DNS poisoning + network position.
//! 3. The daemon validates Origin headers (only futureterm.app/futureterm.com).
//! 4. The daemon binds to 127.0.0.1, not 0.0.0.0.
//! 5. Same trust model as Plex (*.plex.direct), widely deployed.
//!
//! ## Cert lifecycle
//!
//! 1. On startup: load cached cert from disk, check expiry (>7 days remaining).
//! 2. If expired/missing: fetch from GitHub Releases (`cert-latest` tag).
//! 3. If fetch succeeds: save to disk, build TLS acceptor.
//! 4. If fetch fails: return `None` — daemon runs without TLS.
//!    Chrome/Edge users are unaffected (they use WebSerial, not the bridge).
//!    Safari/Firefox users see "bridge unavailable" until cert is available.

use rustls::pki_types::CertificateDer;
use std::path::PathBuf;
use std::sync::Arc;
use tokio_rustls::TlsAcceptor;

/// Cert distribution endpoint. A GitHub Release asset containing the LE
/// cert+key as JSON: `{ cert_pem, key_pem, expires }`.
/// The `cert-latest` tag is updated every 60 days by the `renew-cert` workflow.
const CERT_URL: &str =
    "https://github.com/luofang34/FutureTerm/releases/download/cert-latest/cert-bundle.json";

/// Cert storage directory: ~/Library/Application Support/FutureTerm/ (macOS)
/// or ~/.local/share/FutureTerm/ (Linux).
fn cert_dir() -> Option<PathBuf> {
    dirs::data_dir().map(|d| d.join("FutureTerm"))
}

fn cert_path() -> Option<PathBuf> {
    cert_dir().map(|d| d.join("acme-cert.pem"))
}

fn key_path() -> Option<PathBuf> {
    cert_dir().map(|d| d.join("acme-key.pem"))
}

/// Load a TLS acceptor using the ACME certificate from the cert worker.
///
/// Tries disk cache first (if cert has >7 days until expiry), then fetches
/// from the cert worker. Returns `None` if no valid cert is available —
/// the daemon will run without TLS in that case.
pub async fn load_tls_acceptor() -> Option<TlsAcceptor> {
    // Try cached cert first
    if let Some((cert_pem, key_pem)) = load_cached_cert().await {
        if is_cert_valid(&cert_pem) {
            if let Ok(acceptor) = build_acceptor(&cert_pem, &key_pem) {
                eprintln!("TLS: Using cached ACME certificate for local.futureterm.app");
                return Some(acceptor);
            }
            eprintln!("TLS: Cached certificate failed to load, fetching fresh cert...");
        } else {
            eprintln!("TLS: Cached certificate expired or expiring soon, refreshing...");
        }
    }

    // Fetch from cert worker
    match fetch_acme_cert() {
        Ok((cert_pem, key_pem)) => {
            eprintln!("TLS: Fetched fresh ACME certificate for local.futureterm.app");

            // Save to disk for next startup
            if let Err(e) = save_cert_to_disk(&cert_pem, &key_pem).await {
                eprintln!("TLS: Warning: failed to cache certificate: {}", e);
            }

            match build_acceptor(&cert_pem, &key_pem) {
                Ok(acceptor) => Some(acceptor),
                Err(e) => {
                    eprintln!("TLS: Failed to build acceptor from fetched cert: {}", e);
                    None
                }
            }
        }
        Err(e) => {
            eprintln!("TLS: Failed to fetch ACME certificate: {}", e);
            eprintln!("TLS: Daemon will run without TLS (Safari/Firefox bridge disabled)");
            None
        }
    }
}

/// Fetch the ACME certificate bundle from GitHub Releases.
///
/// Uses `ureq` (blocking HTTP) since this runs once at startup before the
/// async runtime is fully loaded with connections. The release asset is
/// JSON: `{ cert_pem: string, key_pem: string, expires: string }`.
/// ureq follows the GitHub 302 redirect to objects.githubusercontent.com
/// transparently.
fn fetch_acme_cert() -> Result<(String, String), String> {
    let body = ureq::get(CERT_URL)
        .config()
        .timeout_connect(Some(std::time::Duration::from_secs(10)))
        .timeout_recv_body(Some(std::time::Duration::from_secs(10)))
        .build()
        .call()
        .map_err(|e| format!("HTTP request failed: {}", e))?
        .into_body()
        .read_to_string()
        .map_err(|e| format!("Failed to read response: {}", e))?;

    let json: serde_json::Value =
        serde_json::from_str(&body).map_err(|e| format!("Failed to parse JSON: {}", e))?;

    let cert_pem = json
        .get("cert_pem")
        .and_then(serde_json::Value::as_str)
        .ok_or("Missing cert_pem in response")?
        .to_owned();

    let key_pem = json
        .get("key_pem")
        .and_then(serde_json::Value::as_str)
        .ok_or("Missing key_pem in response")?
        .to_owned();

    if cert_pem.is_empty() || key_pem.is_empty() {
        return Err("Empty cert or key in response".into());
    }

    Ok((cert_pem, key_pem))
}

/// Check if a PEM certificate is valid (not expired and >7 days remaining).
///
/// Parses the first certificate in the PEM bundle and checks its `not_after`
/// field. Returns `false` if parsing fails or the cert expires within 7 days.
fn is_cert_valid(cert_pem: &str) -> bool {
    use rustls_pemfile::certs;

    let certs: Vec<CertificateDer<'static>> =
        match certs(&mut cert_pem.as_bytes()).collect::<Result<Vec<_>, _>>() {
            Ok(c) if !c.is_empty() => c,
            _ => return false,
        };

    // Parse the first cert to check expiry using x509-parser
    // We check the raw ASN.1 not_after field
    match parse_cert_expiry(certs.first()) {
        Some(not_after) => {
            let now = std::time::SystemTime::now();
            let seven_days = std::time::Duration::from_secs(7 * 24 * 60 * 60);
            match now.checked_add(seven_days) {
                Some(threshold) => not_after > threshold,
                None => false,
            }
        }
        None => {
            // Can't determine expiry — treat as valid (let build_acceptor validate)
            true
        }
    }
}

/// Extract the `not_after` time from a DER-encoded X.509 certificate.
///
/// Performs minimal ASN.1 parsing to find the validity period without
/// pulling in a full X.509 parser dependency. Returns `None` if parsing fails.
fn parse_cert_expiry(cert: Option<&CertificateDer<'_>>) -> Option<std::time::SystemTime> {
    let cert = cert?;
    let der = cert.as_ref();

    // X.509 structure (simplified):
    //   Certificate ::= SEQUENCE {
    //     tbsCertificate SEQUENCE {
    //       version [0] EXPLICIT ...,
    //       serialNumber INTEGER,
    //       signature AlgorithmIdentifier,
    //       issuer Name,
    //       validity SEQUENCE {
    //         notBefore Time,
    //         notAfter  Time    <-- we want this
    //       }, ...
    //     }, ...
    //   }

    // We'll do a best-effort parse by scanning for the validity SEQUENCE
    // which contains two UTCTime or GeneralizedTime values.
    // For LE certs, notAfter is always UTCTime (tag 0x17).
    parse_not_after_from_der(der)
}

/// Parse notAfter from raw DER bytes of an X.509 certificate.
fn parse_not_after_from_der(der: &[u8]) -> Option<std::time::SystemTime> {
    // Find UTCTime (0x17) or GeneralizedTime (0x18) tags.
    // In a standard X.509 cert, the second time value is notAfter.
    let mut time_count = 0;
    let mut pos = 0;

    while pos < der.len() {
        let tag = *der.get(pos)?;
        pos += 1;

        if tag == 0x17 || tag == 0x18 {
            // Read length
            let (len, new_pos) = read_asn1_length(der, pos)?;
            pos = new_pos;

            time_count += 1;
            if time_count == 2 {
                // This is notAfter
                let time_bytes = der.get(pos..pos + len)?;
                let time_str = std::str::from_utf8(time_bytes).ok()?;
                return parse_asn1_time(tag, time_str);
            }
            pos += len;
        } else {
            // Skip this TLV
            let (len, new_pos) = read_asn1_length(der, pos)?;
            pos = new_pos;

            // For constructed types (bit 5 set), we descend into contents
            // For primitive types, we skip the contents
            if tag & 0x20 != 0 {
                // Constructed — continue parsing inside
            } else {
                pos += len;
            }
        }
    }

    None
}

fn read_asn1_length(der: &[u8], pos: usize) -> Option<(usize, usize)> {
    let first = *der.get(pos)?;
    if first < 0x80 {
        Some((first as usize, pos + 1))
    } else {
        let num_bytes = (first & 0x7f) as usize;
        if num_bytes > 4 || num_bytes == 0 {
            return None;
        }
        let mut len: usize = 0;
        for i in 0..num_bytes {
            len = (len << 8) | (*der.get(pos + 1 + i)? as usize);
        }
        Some((len, pos + 1 + num_bytes))
    }
}

/// Parse ASN.1 UTCTime (YYMMDDHHMMSSZ) or GeneralizedTime (YYYYMMDDHHMMSSZ).
fn parse_asn1_time(tag: u8, s: &str) -> Option<std::time::SystemTime> {
    let s = s.trim_end_matches('Z');

    let (year, rest) = if tag == 0x17 {
        // UTCTime: YY
        let yy: u64 = s.get(..2)?.parse().ok()?;
        let year = if yy >= 50 { 1900 + yy } else { 2000 + yy };
        (year, s.get(2..)?)
    } else {
        // GeneralizedTime: YYYY
        let yyyy: u64 = s.get(..4)?.parse().ok()?;
        (yyyy, s.get(4..)?)
    };

    let month: u64 = rest.get(..2)?.parse().ok()?;
    let day: u64 = rest.get(2..4)?.parse().ok()?;
    let hour: u64 = rest.get(4..6)?.parse().ok()?;
    let min: u64 = rest.get(6..8)?.parse().ok()?;
    let sec: u64 = rest.get(8..10)?.parse().ok()?;

    // Approximate conversion to Unix timestamp (ignoring leap seconds)
    let days_in_months: [u64; 12] = [31, 28, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    let mut days: u64 = 0;

    // Years since 1970
    for y in 1970..year {
        days += if is_leap_year(y) { 366 } else { 365 };
    }

    // Months in current year
    for m in 0..(month.saturating_sub(1) as usize) {
        if m < 12 {
            days += days_in_months.get(m).copied().unwrap_or(30);
            if m == 1 && is_leap_year(year) {
                days += 1;
            }
        }
    }

    days += day.saturating_sub(1);

    let secs = days * 86400 + hour * 3600 + min * 60 + sec;
    Some(std::time::UNIX_EPOCH + std::time::Duration::from_secs(secs))
}

fn is_leap_year(y: u64) -> bool {
    (y.is_multiple_of(4) && !y.is_multiple_of(100)) || y.is_multiple_of(400)
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

/// Load cached ACME cert+key from disk.
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

/// Save ACME cert+key to disk for caching across daemon restarts.
async fn save_cert_to_disk(cert_pem: &str, key_pem: &str) -> Result<(), String> {
    let dir = cert_dir().ok_or("Cannot determine cert directory")?;
    tokio::fs::create_dir_all(&dir)
        .await
        .map_err(|e| format!("Failed to create cert dir: {}", e))?;

    let cert_p = dir.join("acme-cert.pem");
    let key_p = dir.join("acme-key.pem");

    tokio::fs::write(&cert_p, cert_pem)
        .await
        .map_err(|e| format!("Failed to write cert: {}", e))?;

    // Restrict private key file permissions (0o600 on Unix)
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

    #[test]
    fn test_is_cert_valid_empty() {
        assert!(!is_cert_valid(""));
    }

    #[test]
    fn test_is_cert_valid_garbage() {
        assert!(!is_cert_valid("not a certificate"));
    }

    #[test]
    fn test_parse_asn1_utc_time() {
        // 2026-06-15 12:00:00 UTC
        let result = parse_asn1_time(0x17, "260615120000Z");
        assert!(result.is_some());
        let t = result.expect("should parse");
        // Should be in the future from test perspective
        assert!(t > std::time::UNIX_EPOCH);
    }

    #[test]
    fn test_parse_asn1_generalized_time() {
        // 2026-06-15 12:00:00 UTC
        let result = parse_asn1_time(0x18, "20260615120000Z");
        assert!(result.is_some());
    }

    #[test]
    fn test_is_leap_year() {
        assert!(is_leap_year(2024));
        assert!(!is_leap_year(2023));
        assert!(is_leap_year(2000));
        assert!(!is_leap_year(1900));
    }

    #[test]
    fn test_read_asn1_length_short() {
        let data = [0x0a]; // length 10
        assert_eq!(read_asn1_length(&data, 0), Some((10, 1)));
    }

    #[test]
    fn test_read_asn1_length_long() {
        let data = [0x82, 0x01, 0x00]; // length 256
        assert_eq!(read_asn1_length(&data, 0), Some((256, 3)));
    }
}
